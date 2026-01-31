# Commands

```
cargo run --  --lsp-command "/home/desertfox/src/llvm-project/build/bin/clangd --log=verbose --pch-storage=memory --malloc-trim -j 1 --compile-commands-dir=/home/desertfox/research/projects/ffmk/criu/  --limit-results=0  --background-index=false -index-file /home/desertfox/research/projects/ffmk/criu/clangd.dex "  --project-root /home/desertfox/research/projects/ffmk/criu/ --compile-commands /home/desertfox/research/projects/ffmk/criu/compile_commands.json --language c -o criu.aji
```

# Authentication (API keys)

- Start the server with your index database (serve subcommand):
  ```
  cargo run --bin askld -- serve --index /path/to/index.db --format sqlite --port 8080 \
    --database-url postgres://user:pass@127.0.0.1:5432/askl
  ```
  Or via env:
  ```
  ASKL_DATABASE_URL=postgres://user:pass@127.0.0.1:5432/askl \\
  cargo run --bin askld -- serve --index /path/to/index.db --format sqlite --port 8080
  ```
- Create a bootstrap API key from localhost:
  ```
  ASKL_BOOTSTRAP_MODE=true cargo run --bin askld -- auth create-api-key \\
    --email user@example.com --name "admin key" \\
    --expires-at 2026-01-01T00:00:00Z
  ```
- Use the key with protected endpoints (local HTTP dev):
  ```
  ASKL_ALLOW_INSECURE_TOKENS=true \\
  curl -H "Authorization: Bearer askl_<id>.<secret>" http://127.0.0.1:8080/version
  ```
- Revoke a key (local only):
  ```
  ASKL_BOOTSTRAP_MODE=true cargo run --bin askld -- auth revoke-api-key \\
    --token-id <uuid>
  ```
- List keys for a user (local only):
  ```
  ASKL_BOOTSTRAP_MODE=true cargo run --bin askld -- auth list-api-keys \\
    --email user@example.com
  ```

Notes:
- API tokens are rejected over plain HTTP unless `ASKL_ALLOW_INSECURE_TOKENS=true` is set.
- If you're behind a proxy, ensure it forwards `X-Forwarded-Proto: https`.
- Authorization and X-API-Key headers are stripped before request logging.

Docker:
- Set `ASKL_DATABASE_URL` for the container, e.g. `-e ASKL_DATABASE_URL=postgres://user:pass@db:5432/askl`.
