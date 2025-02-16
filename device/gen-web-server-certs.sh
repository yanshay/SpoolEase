#!/bin/bash
# This is a simple script to refresh the certificate chains from the links used in the example,
# and to refresh the self-signed CA and associated certificate + private key pair used for examples.

CERTS_DIR=./src/certs

# Generate a CA and a pair of certificate + private key signed with the CA

# Generate CA certificate
openssl req \
  -x509 \
  -newkey rsa:2048 \
  -keyout $CERTS_DIR/ca_key.pem \
  -out $CERTS_DIR/ca_cert.pem \
  -nodes \
  -days 365 \
  -subj "/CN=device.spoolease.io/O=CA\ Certificate"


# Generate certificate signing request (CSR)
openssl req \
    -newkey rsa:2048 \
    -keyout $CERTS_DIR/web-server-private-key.pem \
    -out $CERTS_DIR/csr.pem \
    -nodes \
    -subj "/CN=device.spoolease.io"

# Sign key with CA certificates from CSR
openssl x509 \
    -req \
    -in $CERTS_DIR/csr.pem \
    -CA $CERTS_DIR/ca_cert.pem \
    -CAkey $CERTS_DIR/ca_key.pem \
    -out $CERTS_DIR/web-server-certificate.pem \
    -CAcreateserial \
    -days 365

# Remove csr
rm $CERTS_DIR/csr.pem
rm $CERTS_DIR/ca_cert.srl

# Remove the CA files, if load on browser then keep
rm $CERTS_DIR/ca_cert.pem
rm $CERTS_DIR/ca_key.pem
