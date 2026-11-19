# Security

## Reporting a vulnerability

Please do not open a public issue with session cookies, `auth.json`, tokens,
passwords, personal file names or O2 account details. Use GitHub's private
security advisory for this repository, or contact the maintainer directly.

## How authentication works

The app does not store your O2 password. The bundled `o2cloud` CLI performs the
OAuth/SMS flow, or accepts the session cookies from the app's own login window,
and stores the session in the CLI's config directory (`auth.json`, readable only
by your user). The app calls the CLI as a subprocess and never logs the session
cookies.

The O2 Cloud SAPI used by the CLI is unofficial. Telefónica can change it at
any time; a broken session or API is not a vulnerability in this app.

## Scope

This is a personal, unofficial client. It is not affiliated with, endorsed by
or supported by Telefónica, O2, Movistar or Synchronoss. Use it only with your
own account and in accordance with the service terms.
