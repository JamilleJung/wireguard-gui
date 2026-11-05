Passwordless helper access needs membership in the 'wireguard' group:
  groupadd -f wireguard && usermod -aG wireguard <user>   (then log in again)
