#!/usr/bin/env bash
# Generate "modern encrypted PEM" fixtures (openssl only; ssh-keygen cannot):
#   * BEGIN ENCRYPTED PRIVATE KEY -- PKCS#8 encrypted (openssl genpkey -aes-256-cbc)
#   * BEGIN EC PRIVATE KEY + Proc-Type/DEK-Info -- SEC1 + legacy PEM encryption (AES-256-CBC)
# Passphrase is the public cgw-test-pass. Throwaway test keys, no real credentials.
#
# Public keys are converted to OpenSSH format (like the other fixtures) so tests can call
# them directly via `PublicKey::from_openssh`. Do not use `ssh-keygen -y -f <key>`: this
# repo lives on /mnt/f (drvfs) where every file is 0777, so ssh-keygen refuses to read
# it ("UNPROTECTED PRIVATE KEY FILE"). Hence: openssl export SPKI -> ssh-keygen -i -m
# PKCS8 to OpenSSH (public keys are not permission-checked).
set -e
cd "$(dirname "$0")"

openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 \
  -aes-256-cbc -pass pass:cgw-test-pass -out rsa-pkcs8-enc.key
openssl pkey -in rsa-pkcs8-enc.key -passin pass:cgw-test-pass -pubout -out /tmp/cgw-spki.pem
ssh-keygen -i -m PKCS8 -f /tmp/cgw-spki.pem > rsa-pkcs8-enc.key.pub

openssl ecparam -name prime256v1 -genkey -noout -out .ec-tmp.pem
openssl ec -in .ec-tmp.pem -aes256 -passout pass:cgw-test-pass -out ec-pem-enc.key
openssl ec -in .ec-tmp.pem -pubout -out /tmp/cgw-spki2.pem
ssh-keygen -i -m PKCS8 -f /tmp/cgw-spki2.pem > ec-pem-enc.key.pub
rm -f .ec-tmp.pem /tmp/cgw-spki.pem /tmp/cgw-spki2.pem

for f in rsa-pkcs8-enc.key ec-pem-enc.key; do
  echo "== $f =="
  head -3 "$f"
done
