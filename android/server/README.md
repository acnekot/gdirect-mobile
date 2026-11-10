# Token server

For when Google's sign-in pages cannot be reached from your phone. Run this on a machine that can reach Google (for example a server abroad). It keeps your refresh token and hands short-lived access tokens to the app; **files never pass through it**, they still go between the phone and Google directly.

## Set up

1. In Google Cloud Console create an OAuth client of type *Desktop app* and enable the Drive API.
2. On the server:

   ```sh
   export GDIRECT_CLIENT_ID=... GDIRECT_CLIENT_SECRET=...
   python3 token_server.py login      # open the link, sign in, paste the final (failed) redirect URL back
   export GDIRECT_ACCESS_KEY=$(openssl rand -hex 24)   # at least 16 characters
   python3 token_server.py serve
   ```

   Or `docker compose up -d` with the same variables in a `.env` file.
3. Put it behind HTTPS (Caddy, nginx, a Cloudflare tunnel) and keep the access key secret. The server listens on `127.0.0.1:8787` by default; change it with `GDIRECT_HOST` and `GDIRECT_PORT`.
4. In the app choose *My server* on the login page and enter the address and the access key.

Ten wrong keys in a minute lock the client out for a while. Headers are never logged. Anyone holding the key can get tokens for your Drive, so treat it like a password and rotate it by restarting with a new `GDIRECT_ACCESS_KEY`.
