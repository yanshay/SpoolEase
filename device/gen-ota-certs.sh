openssl s_client -connect raw.githubusercontent.com:443 -showcerts </dev/null 2>/dev/null | sed -n '/-----BEGIN CERTIFICATE-----/,/-----END CERTIFICATE-----/p' > ./src/certs/raw.githubusercontent.com.pem
openssl x509 -in ./src/certs/raw.githubusercontent.com.pem -text -noout > ./src/certs/raw.githubusercontent.com.info
