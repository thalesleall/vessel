#!/bin/sh
set -eu
echo "dev:$(cat /lab/password)" | chpasswd
install -d -m 700 -o dev -g dev /home/dev/.ssh
install -m 600 -o dev -g dev /lab/authorized_keys /home/dev/.ssh/authorized_keys
echo "Laboratório Vessel: você está em $(hostname)." > /etc/motd
if [ "${LAB_HTTP:-}" = "1" ]; then
  mkdir -p /srv/www
  echo "vessel-lab: servico HTTP interno do $(hostname) OK" > /srv/www/index.html
  busybox httpd -p 8000 -h /srv/www
fi
exec /usr/sbin/sshd -D -e
