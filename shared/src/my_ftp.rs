use core::cmp::min;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ffi::CStr;
use embassy_net::{IpAddress, IpEndpoint, Ipv4Address, tcp::TcpSocket};
use esp_alloc::HeapStats;
use esp_mbedtls::{
    AuthMode, Certificate, ClientSessionConfig, Session, SessionConfig, SessionError, TlsReference,
};
use framework::{debug, info};
use snafu::prelude::*;

const EXTRA_DEBUG: bool = false;

macro_rules! debugex {
    ($($t:tt)*) => {
        if EXTRA_DEBUG {
            debug!($($t)*);
        }
    };
}

// Helper for using Snafu

pub struct DebugWrap<E>(pub E);

impl<E: core::fmt::Debug> core::error::Error for DebugWrap<E> {}

impl<E: core::fmt::Debug> core::fmt::Debug for DebugWrap<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

impl<E: core::fmt::Debug> core::fmt::Display for DebugWrap<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&self.0, f)
    }
}

#[derive(Debug, Default)]
pub struct ControlResponse {
    pub code: i32,
    pub string: String,
}

pub struct MyFtps<'a, T>
where
    T: Into<IpEndpoint>,
{
    control_socket: Option<TcpSocket<'a>>,
    tls: TlsReference<'a>,
    ftp_endpoint: T,
    server_name_cstr: &'a CStr,
    server_ca_chain: Option<Certificate<'a>>,
    control_session: Option<Session<'a, TcpSocket<'a>>>,
    data_session: Option<Session<'a, TcpSocket<'a>>>,
    left_to_retrieve: Option<usize>,
}

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("Failed to connect"))]
    Connect {
        #[snafu(source(from(embassy_net::tcp::ConnectError, DebugWrap)))]
        source: DebugWrap<embassy_net::tcp::ConnectError>,
    },
    Tls {
        #[snafu(source(from(SessionError, DebugWrap)))]
        source: DebugWrap<SessionError>,
    },
    Usage {
        reason: String,
    },
    UnexpectedEof,
    InvalidResponse,
    Ftp {
        response: ControlResponse,
    },
}

impl<'a, T> MyFtps<'a, T>
where
    T: Into<IpEndpoint> + Clone,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        control_socket: TcpSocket<'a>,
        tls: TlsReference<'a>,
        ftp_endpoint: T,
        server_name_cstr: &'a CStr,
        server_ca_chain: Option<Certificate<'a>>,
    ) -> MyFtps<'a, T>
    where
        T: Into<IpEndpoint> + Clone,
    {
        MyFtps {
            control_socket: Some(control_socket),
            tls,
            ftp_endpoint,
            server_name_cstr,
            server_ca_chain,
            control_session: None,
            data_session: None,
            left_to_retrieve: None,
        }
    }

    pub async fn connect(&mut self) -> Result<(), Error> {
        info!("Connecting to ftp...");
        self.control_socket
            .as_mut()
            .unwrap()
            .connect(self.ftp_endpoint.clone())
            .await
            .context(ConnectSnafu)?;

        info!("Connected to ftp");
        let auth_mode = if self.server_ca_chain.is_some() {
            AuthMode::Required
        } else {
            AuthMode::None
        };
        let session_config = ClientSessionConfig {
            ca_chain: self.server_ca_chain.clone(),
            auth_mode,
            server_name: Some(self.server_name_cstr),
            ..ClientSessionConfig::new()
        };

        let control_socket = self.control_socket.take().unwrap();

        info!("Establishing TLS connection with ftp...");
        let mut session = Session::new(
            self.tls,
            control_socket,
            &SessionConfig::Client(session_config),
        )
        .context(TlsSnafu)?;

        session.connect().await.context(TlsSnafu)?;
        info!("Established TLS connection with ftp");

        self.control_session = Some(session);

        let response = self.read_decoded_control_response().await?;
        if response.code != 220 {
            return Err(Error::Ftp { response });
        }

        Ok(())
    }

    pub async fn login(&mut self, user: &str, password: &str) -> Result<bool, Error> {
        debugex!(">>>> FTP Login");
        let user_response = self
            .control_transaction(&format!("USER {user}\r\n"))
            .await?;
        match user_response.code {
            230 => return Ok(true),
            331 => (),
            _ => {
                return Err(Error::Ftp {
                    response: user_response,
                });
            }
        }
        // user ok, continue to password
        let pswd_response = self
            .control_transaction(&format!("PASS {password}\r\n"))
            .await?;
        match pswd_response.code {
            230 => Ok(true),
            530 => Ok(false),
            _ => Err(Error::Ftp {
                response: user_response,
            }),
        }
    }

    pub async fn quit(&mut self) -> Result<bool, Error> {
        debugex!(">>> FTP Quit");
        let quit_response = self.control_transaction("QUIT\r\n").await?;
        match quit_response.code {
            -1 => Ok(false),
            221 => Ok(true),
            _ => Err(Error::Ftp {
                response: quit_response,
            }),
        }
    }

    pub async fn noop(&mut self) -> Result<bool, Error> {
        let noop_response = self.control_transaction("NOOP\r\n").await?;
        match noop_response.code {
            220 => Ok(true),
            _ => Err(Error::Ftp {
                response: noop_response,
            }),
        }
    }

    pub async fn start_retrieve_first_of<'b, 'c>(
        &mut self,
        paths: &[String],
        mut data_socket: TcpSocket<'b>,
        memory_save: bool,
    ) -> Result<Option<usize>, Error>
    where
        'b: 'a,
    {
        // inject this to test VSFTPD - this file is there not in the root:
        // let paths = &[String::from("Cube + Cube.3mf")]; // TODO:  remove !!!!!
        debugex!(">>>> Ftp start_retrieve");
        debugex!(">>>> memory_save = {}", memory_save);
        if self.control_session.is_none() {
            return Err(Error::Usage {
                reason: "Can't start_retrieve w/o an open control channel".to_string(),
            });
        }
        // This is a bit tricky so save memory
        // We close the control session before initiating the data session.
        // It seems printer ftp anyway close the control channel when data streaming start
        // So order of things change a bit, so at any given time only one TLS session takes memory
        // And it seems to work with P1S at least
        let response = self.control_transaction("PASV\r\n").await?;
        if response.code != 227 {
            return Err(Error::Ftp { response });
        }

        let mut pasv_result = Self::parse_pasv(&response.string)?;

        if pasv_result.0.octets() == [0, 0, 0, 0] {
            let control_endpoint: IpEndpoint = self.ftp_endpoint.clone().into();
            if let IpAddress::Ipv4(octets) = control_endpoint.addr {
                pasv_result.0 = octets;
            }
        }

        let _response = self.control_transaction("PBSZ 0\r\n").await?;

        let _response = self.control_transaction("PROT P\r\n").await?;

        let mut retr_response = ControlResponse::default();
        let mut retrieve_started = !memory_save;

        for path in paths {
            debugex!(">>>> Trying to read {path}");
            self.write_control(&format!("RETR {path}\r\n")).await?; // only send request at this time (vsftpd sequence)

            if memory_save {
                retr_response = self.read_decoded_control_response().await?;
                if retr_response.code == 550 {
                    continue;
                } else if retr_response.code != 150 {
                    return Err(Error::Ftp {
                        response: retr_response,
                    });
                } else {
                    retrieve_started = true;
                    break;
                }
            };
        }

        if !retrieve_started {
            return Err(Error::Ftp {
                response: retr_response,
            });
        }

        data_socket
            .connect(pasv_result)
            .await
            .context(ConnectSnafu)?;

        if !memory_save {
            retr_response = self.read_decoded_control_response().await?;
            if retr_response.code != 150 {
                data_socket.abort();
                let _ = data_socket.flush().await;
                return Err(Error::Ftp {
                    response: retr_response,
                });
            }
        };

        let stats: HeapStats = esp_alloc::HEAP.stats();
        debug!("1. {}", stats);

        let reused_session = self
            .control_session
            .as_ref()
            .unwrap()
            .save()
            .context(TlsSnafu)?; // can unwrap since testing at fn start control_session is available
        debugex!(">>> Got reused_session");

        if memory_save {
            self.close().await?;
            self.control_session = None; // clear session memory before allocating new session to save memory, not exactly by ftp spec, but work
        }

        let stats: HeapStats = esp_alloc::HEAP.stats();
        debug!("2. {}", stats);

        let auth_mode = if self.server_ca_chain.is_some() {
            AuthMode::Required
        } else {
            AuthMode::None
        };
        let session_config = ClientSessionConfig {
            ca_chain: self.server_ca_chain.clone(),
            auth_mode,
            server_name: Some(self.server_name_cstr),
            ..ClientSessionConfig::new()
        };
        let mut data_session = Session::new(
            self.tls,
            data_socket,
            &SessionConfig::Client(session_config),
        )
        .context(TlsSnafu)?;

        data_session
            .connect_with_session(&reused_session)
            .await
            .context(TlsSnafu)?;

        debugex!(">>>> Connected using reused session");

        let stats: HeapStats = esp_alloc::HEAP.stats();
        debug!("3. {}", stats);

        self.data_session = Some(data_session);

        fn get_num_bytes_in_parenthesis(s: &str) -> Option<usize> {
            let (start, end) = (s.find('(')?, s.find(')')?);
            let inner = &s[start + 1..end];
            inner.split_whitespace().next()?.parse().ok()
        }

        if let Some(file_length) = get_num_bytes_in_parenthesis(&retr_response.string) {
            self.left_to_retrieve = Some(file_length);
            Ok(Some(file_length))
        } else {
            Ok(None)
        }
    }

    pub async fn retrieve(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        // if let Some(left_to_retrieve) = self.left_to_retrieve {
        //     if left_to_retrieve < 32768 {
        //         self.write_control("NOOP\r\n").await?;
        //     }
        // }

        // This function supports both case when RETR response contains file length and when not.
        // When it contains file length (x1c, vsftpd), we fetch from ftp until size depleted, next read will only end_retrieve and return 0
        // When it doen't (p1s), we fetch until we get 0 and on zero we do the end_retrieve
        // From client POV it is the same behavior, but here in the code is't different
        let left_to_retrieve = self.left_to_retrieve;
        debugex!(">>>> left_to_retrieve = {left_to_retrieve:?}");
        if left_to_retrieve == Some(0) && self.data_session.is_some() {
            debugex!(">>> This is the last read of data, calling end_retrieve");
            self.end_retrieve().await.map(|()| 0)
        } else {
            let res = if let Some(data_session) = &mut self.data_session {
                let buf = if let Some(left_to_retrieve) = self.left_to_retrieve {
                    debugex!(">>>> using shorter buffer size {left_to_retrieve}");
                    // TODO: I don't think it is required, but trying
                    let real_len = min(left_to_retrieve, buf.len());
                    &mut buf[..real_len]
                } else {
                    buf
                };
                match data_session.read(buf).await {
                    Ok(n) => {
                        if n == 0 {
                            // a case when 0 returned without file length
                            debugex!(">>>> Read 0 bytes, calling end_retrieve");
                            self.end_retrieve().await?;
                        }
                        Ok(n)
                    }
                    Err(err) => Err(Error::Tls {
                        source: DebugWrap(err),
                    }),
                }
            } else {
                Err(Error::Usage {
                    reason: "retrieve w/o data session".to_string(),
                })
            };

            if let Ok(n) = res
                && self.left_to_retrieve.is_some()
            {
                self.left_to_retrieve = Some(left_to_retrieve.unwrap() - n);
            }
            res
        }
    }

    pub async fn end_retrieve(&mut self) -> Result<(), Error> {
        debugex!(">>>> FTP end_retrieve");
        if let Some(mut data_session) = self.data_session.take() {
            debugex!(">>>> left_to_retrieve : {:?}", self.left_to_retrieve);
            // if self.left_to_retrieve != Some(0) {
            //     debugex!(">>> end_retrieve ABOR flow, sending ABOR");
            //     self.write_control("ABOR\r\n").await?;
            //     let mut buf = alloc::vec![0;16384];
            //     loop {
            //         match data_session.read(&mut buf).await {
            //             Ok(n) => {
            //                 if n == 0 {
            //                     // a case when 0 returned without file length
            //                     debugex!(">>>> Read 0 bytes, moving on");
            //                     break;
            //                 }
            //                 debugex!(">>>>> Read {n} bytes after abort, trying again. Maybe should stop if less than buffer size?")
            //             }
            //             Err(err) => {
            //                 debugex!(">>>>> read errored {err:?} after abort, expected, move on");
            //                 break;
            //             }
            //         }
            //     }
            // }

            debugex!(">>> closing data_session in end_retrieve");
            let _ = data_session.close().await;

            // debugex!(">>> dropping data_session in end_retrieve");
            // drop(data_session);

            debugex!(">>> trying to read response");
            let response = self.read_decoded_control_response().await?;
            debugex!(">>> received response code {}", response.code);

            // if self.left_to_retrieve != Some(0) {
            //     debugex!(">>> end_retrieve continue ABOR flow");
            //     if response.code != 226 && response.code != 426 {
            //         return Err(Error::Ftp { response });
            //     }
            //     debugex!(">>> waiting for 225, or anything else");
            //     let _response = self.read_decoded_control_response().await?;
            // } else {
            debugex!(">>> not using ABOR flow");
            if response.code != 226 {
                return Err(Error::Ftp { response });
            }
            // }
            Ok(())
        } else {
            // Err(Error::Usage {
            //     reason: "end_retrieve w/o data session".to_string(),
            // })
            Ok(())
        }
    }

    fn parse_pasv(s: &str) -> Result<(Ipv4Address, u16), Error> {
        let start = s.find('(').ok_or(Error::InvalidResponse)? + 1;
        let end = s.find(')').ok_or(Error::InvalidResponse)?;
        let parts: Vec<u8> = s[start..end]
            .split(',')
            .filter_map(|x| x.trim().parse().ok())
            .collect();

        if parts.len() != 6 {
            return Err(Error::InvalidResponse);
        }

        let ip = Ipv4Address::new(parts[0], parts[1], parts[2], parts[3]);
        let port = (parts[4] as u16) << 8 | (parts[5] as u16);
        Ok((ip, port))
    }

    async fn control_transaction(&mut self, command: &str) -> Result<ControlResponse, Error> {
        self.write_control(command).await?;
        let response = self.read_decoded_control_response().await?;
        Ok(response)
    }
    async fn write_control(&mut self, command: &str) -> Result<(), Error> {
        debugex!(">>>> ---> {command}");
        if let Some(session) = self.control_session.as_mut() {
            session.write(command.as_bytes()).await.context(TlsSnafu)?;
            session.flush().await.context(TlsSnafu)?;
        }
        Ok(())
    }

    async fn read_decoded_control_response(&mut self) -> Result<ControlResponse, Error> {
        let response_bytes = self.read_control_response().await?;
        if let Some(response_bytes) = response_bytes {
            let string = String::from_utf8(response_bytes).map_err(|_| Error::InvalidResponse)?;
            debugex!(">>>> <--- {string}");
            let code_str = string.split(' ').next().unwrap(); // read_control would fail if no space
            let code = code_str
                .parse::<i32>()
                .map_err(|_| Error::InvalidResponse)?;
            Ok(ControlResponse { code, string })
        } else {
            debugex!(">>>> <---- expected response but received None");
            Ok(ControlResponse {
                code: -1,
                string: "Control channel already close".to_string(),
            })
        }
    }
    async fn read_control_response(&mut self) -> Result<Option<Vec<u8>>, Error> {
        debugex!(">>>> read_control_response");
        if let Some(session) = self.control_session.as_mut() {
            let mut buf = Vec::new();
            let mut tmp = [0u8; 64];
            let mut lines = Vec::new();

            let (mut code, mut multiline) = ([0; 3], false);

            loop {
                let n = session.read(&mut tmp).await.context(TlsSnafu)?;
                if n == 0 {
                    debugex!(">>>> received unexpected eof");
                    return Err(Error::UnexpectedEof);
                }
                buf.extend_from_slice(&tmp[..n]);

                while let Some(pos) = buf.windows(2).position(|w| w == b"\r\n") {
                    let line = buf.drain(..pos + 2).collect::<Vec<u8>>();
                    if lines.is_empty() {
                        // first line
                        if line.len() < 4 {
                            return Err(Error::InvalidResponse);
                        }
                        code.copy_from_slice(&line[..3]);
                        multiline = line[3] == b'-';
                    }
                    let is_end = line.len() >= 4 && line[..3] == code && line[3] == b' ';
                    lines.extend(line);
                    if !multiline || is_end {
                        return Ok(Some(lines));
                    }
                }
            }
        } else {
            Ok(None)
        }
    }

    pub async fn close(&mut self) -> Result<(), Error> {
        debugex!(">>> MyFtp::close");
        if self.control_session.is_some() {
            debugex!(">>>> closing control_session");
            let mut control_session = self.control_session.take().unwrap();
            control_session.close().await.context(TlsSnafu)?;
        }
        if self.data_session.is_some() {
            debugex!(">>>> closing data_session");
            let mut data_session = self.data_session.take().unwrap();
            data_session.close().await.context(TlsSnafu)?;
        }
        Ok(())
    }
}
