use alloc::{string::String, vec, vec::Vec};
use core::{ffi::{c_int, c_uchar, c_void}, net::Ipv4Addr, str::FromStr};

use esp_mbedtls::sys::*;

const KEY_PEM_BUFFER_LEN: usize = 2048;
const CERT_PEM_BUFFER_LEN: usize = 4096;

pub const DEFAULT_CA_SUBJECT: &str = "CN=SpoolEase Device CA,O=SpoolEase";

#[derive(Debug)]
pub struct CertError {
    pub context: &'static str,
    pub code: c_int,
}

impl CertError {
    const fn new(context: &'static str, code: c_int) -> Self {
        Self { context, code }
    }
}

pub struct GeneratedDeviceCertificate {
    pub ca_key_pem: String,
    pub ca_cert_pem: String,
    pub leaf_key_pem: String,
    pub leaf_cert_pem: String,
}

pub struct LeafCertificate {
    pub leaf_key_pem: String,
    pub leaf_cert_pem: String,
}

pub struct CertificateValidity<'a> {
    pub not_before: &'a str,
    pub not_after: &'a str,
}

pub fn generate_ca_and_leaf(
    ca_subject: &str,
    ca_validity: &CertificateValidity<'_>,
    leaf_subject: &str,
    leaf_validity: &CertificateValidity<'_>,
    sans: &[String],
) -> Result<GeneratedDeviceCertificate, CertError> {
    let mut drbg = DrbgContext::new()?;
    let mut ca_key = PkContext::generate_ecdsa_p256(&mut drbg)?;
    let mut ca_cert = WriteCert::new();
    ca_cert.configure_self_signed_ca(&mut ca_key, ca_subject, ca_validity)?;

    let ca_key_pem = ca_key.write_private_key_pem()?;
    let ca_cert_pem = ca_cert.write_pem(&mut drbg)?;

    let leaf = issue_leaf_from_ca_key(&mut drbg, &mut ca_key, ca_subject, leaf_subject, leaf_validity, sans)?;

    Ok(GeneratedDeviceCertificate {
        ca_key_pem,
        ca_cert_pem,
        leaf_key_pem: leaf.leaf_key_pem,
        leaf_cert_pem: leaf.leaf_cert_pem,
    })
}

pub fn issue_leaf_from_existing_ca(
    ca_key_pem: &str,
    ca_subject: &str,
    leaf_subject: &str,
    leaf_validity: &CertificateValidity<'_>,
    sans: &[String],
) -> Result<LeafCertificate, CertError> {
    let mut drbg = DrbgContext::new()?;
    let mut ca_key = PkContext::parse_private_key(ca_key_pem, &mut drbg)?;
    issue_leaf_from_ca_key(&mut drbg, &mut ca_key, ca_subject, leaf_subject, leaf_validity, sans)
}

fn issue_leaf_from_ca_key(
    drbg: &mut DrbgContext,
    ca_key: &mut PkContext,
    ca_subject: &str,
    leaf_subject: &str,
    leaf_validity: &CertificateValidity<'_>,
    sans: &[String],
) -> Result<LeafCertificate, CertError> {
    let mut leaf_key = PkContext::generate_ecdsa_p256(drbg)?;
    let mut leaf_cert = WriteCert::new();
    leaf_cert.configure_leaf(&mut leaf_key, ca_key, ca_subject, leaf_subject, leaf_validity, sans)?;

    Ok(LeafCertificate {
        leaf_key_pem: leaf_key.write_private_key_pem()?,
        leaf_cert_pem: leaf_cert.write_pem(drbg)?,
    })
}

struct DrbgContext {
    inner: mbedtls_ctr_drbg_context,
}

impl DrbgContext {
    fn new() -> Result<Self, CertError> {
        let mut inner = unsafe { core::mem::zeroed::<mbedtls_ctr_drbg_context>() };
        let personalization = c_string_bytes("spoolease-certgen", "personalization")?;

        unsafe { mbedtls_ctr_drbg_init(&mut inner) };
        check_mbedtls("mbedtls_ctr_drbg_seed", unsafe {
            mbedtls_ctr_drbg_seed(
                &mut inner,
                Some(rng_callback),
                core::ptr::null_mut(),
                personalization.as_ptr(),
                personalization.len() - 1,
            )
        })?;

        Ok(Self { inner })
    }

    fn random_ctx(&mut self) -> *mut c_void {
        (&mut self.inner as *mut mbedtls_ctr_drbg_context).cast()
    }
}

impl Drop for DrbgContext {
    fn drop(&mut self) {
        unsafe { mbedtls_ctr_drbg_free(&mut self.inner) };
    }
}

struct PkContext {
    inner: mbedtls_pk_context,
}

impl PkContext {
    fn new() -> Self {
        let mut inner = unsafe { core::mem::zeroed::<mbedtls_pk_context>() };
        unsafe { mbedtls_pk_init(&mut inner) };
        Self { inner }
    }

    fn generate_ecdsa_p256(drbg: &mut DrbgContext) -> Result<Self, CertError> {
        let mut key = Self::new();
        let info = unsafe { mbedtls_pk_info_from_type(mbedtls_pk_type_t_MBEDTLS_PK_ECKEY) };
        if info.is_null() {
            return Err(CertError::new("mbedtls_pk_info_from_type", -1));
        }

        check_mbedtls("mbedtls_pk_setup", unsafe { mbedtls_pk_setup(&mut key.inner, info) })?;

        let ecp = key.inner.private_pk_ctx.cast::<mbedtls_ecp_keypair>();
        check_mbedtls("mbedtls_ecp_gen_key", unsafe {
            mbedtls_ecp_gen_key(
                mbedtls_ecp_group_id_MBEDTLS_ECP_DP_SECP256R1,
                ecp,
                Some(mbedtls_ctr_drbg_random),
                drbg.random_ctx(),
            )
        })?;

        Ok(key)
    }

    fn parse_private_key(pem: &str, drbg: &mut DrbgContext) -> Result<Self, CertError> {
        let mut key = Self::new();
        let pem = c_string_bytes(pem, "ca private key pem")?;

        check_mbedtls("mbedtls_pk_parse_key", unsafe {
            mbedtls_pk_parse_key(
                &mut key.inner,
                pem.as_ptr(),
                pem.len(),
                core::ptr::null(),
                0,
                Some(mbedtls_ctr_drbg_random),
                drbg.random_ctx(),
            )
        })?;

        Ok(key)
    }

    fn as_mut_ptr(&mut self) -> *mut mbedtls_pk_context {
        &mut self.inner
    }

    fn write_private_key_pem(&mut self) -> Result<String, CertError> {
        let mut buffer = vec![0_u8; KEY_PEM_BUFFER_LEN];
        check_mbedtls("mbedtls_pk_write_key_pem", unsafe {
            mbedtls_pk_write_key_pem(&self.inner, buffer.as_mut_ptr(), buffer.len())
        })?;
        pem_string_from_buffer(&buffer, "private key pem")
    }
}

impl Drop for PkContext {
    fn drop(&mut self) {
        unsafe { mbedtls_pk_free(&mut self.inner) };
    }
}

struct WriteCert {
    inner: mbedtls_x509write_cert,
}

impl WriteCert {
    fn new() -> Self {
        let mut inner = unsafe { core::mem::zeroed::<mbedtls_x509write_cert>() };
        unsafe { mbedtls_x509write_crt_init(&mut inner) };
        Self { inner }
    }

    fn configure_self_signed_ca(
        &mut self,
        key: &mut PkContext,
        subject: &str,
        validity: &CertificateValidity<'_>,
    ) -> Result<(), CertError> {
        self.configure_common(key.as_mut_ptr(), key.as_mut_ptr(), subject, subject, validity)?;

        check_mbedtls("mbedtls_x509write_crt_set_basic_constraints", unsafe {
            mbedtls_x509write_crt_set_basic_constraints(&mut self.inner, 1, 0)
        })?;
        check_mbedtls("mbedtls_x509write_crt_set_key_usage", unsafe {
            mbedtls_x509write_crt_set_key_usage(&mut self.inner, MBEDTLS_X509_KU_KEY_CERT_SIGN | MBEDTLS_X509_KU_CRL_SIGN)
        })?;
        check_mbedtls("mbedtls_x509write_crt_set_subject_key_identifier", unsafe {
            mbedtls_x509write_crt_set_subject_key_identifier(&mut self.inner)
        })?;
        check_mbedtls("mbedtls_x509write_crt_set_authority_key_identifier", unsafe {
            mbedtls_x509write_crt_set_authority_key_identifier(&mut self.inner)
        })
    }

    fn configure_leaf(
        &mut self,
        leaf_key: &mut PkContext,
        ca_key: &mut PkContext,
        ca_subject: &str,
        leaf_subject: &str,
        validity: &CertificateValidity<'_>,
        sans: &[String],
    ) -> Result<(), CertError> {
        self.configure_common(leaf_key.as_mut_ptr(), ca_key.as_mut_ptr(), leaf_subject, ca_subject, validity)?;

        check_mbedtls("mbedtls_x509write_crt_set_basic_constraints", unsafe {
            mbedtls_x509write_crt_set_basic_constraints(&mut self.inner, 0, -1)
        })?;
        check_mbedtls("mbedtls_x509write_crt_set_key_usage", unsafe {
            mbedtls_x509write_crt_set_key_usage(&mut self.inner, MBEDTLS_X509_KU_DIGITAL_SIGNATURE)
        })?;
        check_mbedtls("mbedtls_x509write_crt_set_subject_key_identifier", unsafe {
            mbedtls_x509write_crt_set_subject_key_identifier(&mut self.inner)
        })?;
        check_mbedtls("mbedtls_x509write_crt_set_authority_key_identifier", unsafe {
            mbedtls_x509write_crt_set_authority_key_identifier(&mut self.inner)
        })?;

        self.set_subject_alt_names(sans)?;
        self.set_server_auth_ext_key_usage()
    }

    fn configure_common(
        &mut self,
        subject_key: *mut mbedtls_pk_context,
        issuer_key: *mut mbedtls_pk_context,
        subject_name: &str,
        issuer_name: &str,
        validity: &CertificateValidity<'_>,
    ) -> Result<(), CertError> {
        let subject_name = c_string_bytes(subject_name, "subject_name")?;
        let issuer_name = c_string_bytes(issuer_name, "issuer_name")?;
        let not_before = x509_time_bytes(validity.not_before, "not_before")?;
        let not_after = x509_time_bytes(validity.not_after, "not_after")?;
        let mut serial = random_serial()?;

        unsafe {
            mbedtls_x509write_crt_set_version(&mut self.inner, MBEDTLS_X509_CRT_VERSION_3 as c_int);
            mbedtls_x509write_crt_set_md_alg(&mut self.inner, mbedtls_md_type_t_MBEDTLS_MD_SHA256);
            mbedtls_x509write_crt_set_subject_key(&mut self.inner, subject_key);
            mbedtls_x509write_crt_set_issuer_key(&mut self.inner, issuer_key);
        }

        check_mbedtls("mbedtls_x509write_crt_set_serial_raw", unsafe {
            mbedtls_x509write_crt_set_serial_raw(&mut self.inner, serial.as_mut_ptr(), serial.len())
        })?;
        check_mbedtls("mbedtls_x509write_crt_set_subject_name", unsafe {
            mbedtls_x509write_crt_set_subject_name(&mut self.inner, subject_name.as_ptr().cast())
        })?;
        check_mbedtls("mbedtls_x509write_crt_set_issuer_name", unsafe {
            mbedtls_x509write_crt_set_issuer_name(&mut self.inner, issuer_name.as_ptr().cast())
        })?;
        check_mbedtls("mbedtls_x509write_crt_set_validity", unsafe {
            mbedtls_x509write_crt_set_validity(&mut self.inner, not_before.as_ptr().cast(), not_after.as_ptr().cast())
        })
    }

    fn set_subject_alt_names(&mut self, sans: &[String]) -> Result<(), CertError> {
        let mut values: Vec<Vec<u8>> = sans
            .iter()
            .map(|san| san_to_bytes(san))
            .collect::<Result<Vec<_>, _>>()?;
        let mut nodes = vec![mbedtls_x509_san_list::default(); values.len()];
        let nodes_ptr = nodes.as_mut_ptr();

        for i in 0..nodes.len() {
            let is_ip = Ipv4Addr::from_str(sans[i].as_str()).is_ok();
            nodes[i].node.type_ = if is_ip { MBEDTLS_X509_SAN_IP_ADDRESS } else { MBEDTLS_X509_SAN_DNS_NAME } as c_int;
            nodes[i].node.san.unstructured_name = mbedtls_x509_buf {
                tag: 0,
                len: values[i].len(),
                p: values[i].as_mut_ptr(),
            };
            nodes[i].next = if i + 1 < nodes.len() {
                unsafe { nodes_ptr.add(i + 1) }
            } else {
                core::ptr::null_mut()
            };
        }

        check_mbedtls("mbedtls_x509write_crt_set_subject_alternative_name", unsafe {
            mbedtls_x509write_crt_set_subject_alternative_name(&mut self.inner, nodes.as_ptr())
        })
    }

    fn set_server_auth_ext_key_usage(&mut self) -> Result<(), CertError> {
        let mut oid = [0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];
        let sequence = mbedtls_asn1_sequence {
            buf: mbedtls_asn1_buf {
                tag: MBEDTLS_ASN1_OID as c_int,
                len: oid.len(),
                p: oid.as_mut_ptr(),
            },
            next: core::ptr::null_mut(),
        };

        check_mbedtls("mbedtls_x509write_crt_set_ext_key_usage", unsafe {
            mbedtls_x509write_crt_set_ext_key_usage(&mut self.inner, &sequence)
        })
    }

    fn write_pem(&mut self, drbg: &mut DrbgContext) -> Result<String, CertError> {
        let mut buffer = vec![0_u8; CERT_PEM_BUFFER_LEN];
        check_mbedtls("mbedtls_x509write_crt_pem", unsafe {
            mbedtls_x509write_crt_pem(
                &mut self.inner,
                buffer.as_mut_ptr(),
                buffer.len(),
                Some(mbedtls_ctr_drbg_random),
                drbg.random_ctx(),
            )
        })?;
        pem_string_from_buffer(&buffer, "certificate pem")
    }
}

impl Drop for WriteCert {
    fn drop(&mut self) {
        unsafe { mbedtls_x509write_crt_free(&mut self.inner) };
    }
}

fn san_to_bytes(value: &str) -> Result<Vec<u8>, CertError> {
    if value.as_bytes().contains(&0) || value.trim().is_empty() {
        return Err(CertError::new("invalid_san", -10));
    }
    if let Ok(ip) = Ipv4Addr::from_str(value) {
        return Ok(ip.octets().to_vec());
    }
    Ok(value.as_bytes().to_vec())
}

fn random_serial() -> Result<[u8; 16], CertError> {
    let mut serial = [0_u8; 16];
    getrandom::getrandom(&mut serial).map_err(|_| CertError::new("getrandom", -20))?;
    serial[0] &= 0x7f;
    if serial[0] == 0 {
        serial[0] = 1;
    }
    Ok(serial)
}

fn x509_time_bytes(value: &str, context: &'static str) -> Result<Vec<u8>, CertError> {
    if value.len() != 14 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(CertError::new(context, -30));
    }
    c_string_bytes(value, context)
}

fn c_string_bytes(value: &str, context: &'static str) -> Result<Vec<u8>, CertError> {
    if value.as_bytes().contains(&0) {
        return Err(CertError::new(context, -31));
    }
    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0);
    Ok(bytes)
}

fn pem_string_from_buffer(buffer: &[u8], context: &'static str) -> Result<String, CertError> {
    let len = buffer
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| CertError::new(context, -32))?;
    String::from_utf8(buffer[..len].to_vec()).map_err(|_| CertError::new(context, -33))
}

fn check_mbedtls(context: &'static str, code: c_int) -> Result<(), CertError> {
    if code == 0 { Ok(()) } else { Err(CertError::new(context, code)) }
}

unsafe extern "C" fn rng_callback(_ctx: *mut c_void, output: *mut c_uchar, len: usize) -> c_int {
    let out = unsafe { core::slice::from_raw_parts_mut(output, len) };
    match getrandom::getrandom(out) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

impl core::fmt::Display for CertError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} ({:#x})", self.context, self.code)
    }
}

impl core::error::Error for CertError {}
