#!/bin/sh
# One-shot: creates the lab's test credentials in /lab (mounted from deploy/dev/.lab) once.
set -eu
cd /lab
if [ -f password ]; then echo "keygen: credentials already exist"; exit 0; fi
# Exactly $1 alphanumeric characters: keep drawing until there are enough (a fixed 64-byte
# draw filtered to [A-Za-z0-9] can come up short).
rand() { tr -dc 'A-Za-z0-9' < /dev/urandom | head -c "$1"; }
rand 16 > password
rand 16 > passphrase
rand 20 > bot-password
ssh-keygen -q -t ed25519 -N "" -C "vessel-lab-sem-passphrase" -f id_ed25519
ssh-keygen -q -t ed25519 -N "$(cat passphrase)" -C "vessel-lab-com-passphrase" -f id_ed25519_passphrase
cat id_ed25519.pub id_ed25519_passphrase.pub > authorized_keys
cat > CREDENCIAIS.txt <<TXT
Credenciais de TESTE do laboratório Vessel (descartáveis; não use em nada real).

Usuário SSH ............ dev
Senha SSH .............. $(cat password)
Chave sem passphrase ... id_ed25519
Chave com passphrase ... id_ed25519_passphrase
Passphrase ............. $(cat passphrase)

Conta de teste (vessel-dev) ... bot@vessel.test
Senha da conta de teste ....... $(cat bot-password)
TXT
chmod 600 ./*
chmod 644 ./*.pub authorized_keys
echo "keygen: created test credentials in deploy/dev/.lab"
