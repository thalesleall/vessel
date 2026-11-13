# Throwaway SSH servers for testing Vessel by hand. Not for production.
FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
RUN apt-get update \
 && apt-get install -y --no-install-recommends openssh-server openssh-client busybox curl vim-tiny htop locales \
 && rm -rf /var/lib/apt/lists/* \
 && sed -i 's/^# *pt_BR.UTF-8/pt_BR.UTF-8/; s/^# *en_US.UTF-8/en_US.UTF-8/' /etc/locale.gen && locale-gen && echo "LANG=pt_BR.UTF-8" > /etc/default/locale \
 && useradd -m -s /bin/bash dev \
 && mkdir -p /run/sshd \
 && ssh-keygen -A
COPY sshd_config /etc/ssh/sshd_config.d/lab.conf
COPY entrypoint.sh keygen.sh /usr/local/bin/
RUN chmod +x /usr/local/bin/entrypoint.sh /usr/local/bin/keygen.sh
ENV LANG=pt_BR.UTF-8
EXPOSE 22
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
