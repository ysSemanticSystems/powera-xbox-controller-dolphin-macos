# Repo-local git hooks

This repo uses a local `commit-msg` hook to prevent unwanted commit-message trailers
from being added (for example: `Made-with: Cursor`).

To enable:

```bash
git config core.hooksPath .githooks
chmod +x .githooks/commit-msg
```

This does **not** change your global git config.

