# Manual SSH Key Setup

Use this guide if you prefer to set up SSH key authentication manually, or if the automatic setup didn't work.

## Step 1 — Generate a key on your computer

On Windows (PowerShell):
```powershell
ssh-keygen -t ed25519 -f $HOME\.ssh\winmux_key -C "winmux-<server-name>"
```

On macOS or Linux:
```bash
ssh-keygen -t ed25519 -f ~/.ssh/winmux_key -C "winmux-<server-name>"
```

When prompted for a passphrase, press Enter for no passphrase, or set one for extra security.

## Step 2 — Copy your public key

On Windows (PowerShell):
```powershell
Get-Content $HOME\.ssh\winmux_key.pub | Set-Clipboard
```

On macOS:
```bash
pbcopy < ~/.ssh/winmux_key.pub
```

On Linux:
```bash
cat ~/.ssh/winmux_key.pub
```

## Step 3 — Install the key on the server

Connect to your server (any way that works — password SSH, web console, cloud provider's terminal), then run:

```bash
mkdir -p ~/.ssh
chmod 700 ~/.ssh
echo "PASTE_YOUR_PUBLIC_KEY_HERE" >> ~/.ssh/authorized_keys
chmod 600 ~/.ssh/authorized_keys
```

Replace `PASTE_YOUR_PUBLIC_KEY_HERE` with the public key from Step 2. The whole key is one line that starts with `ssh-ed25519`.

If you have `ssh-copy-id`, you can combine steps 2 and 3:
```bash
ssh-copy-id -i ~/.ssh/winmux_key.pub user@server.example.com
```

## Step 4 — Configure winmux

Open the workspace settings and set the SSH key path to `~/.ssh/winmux_key` (or wherever you saved your private key).

## Troubleshooting

- **Permission denied (publickey)**: Verify file permissions — `~/.ssh` should be 700, `authorized_keys` should be 600, the private key should be 600.
- **Wrong user on the server**: Make sure you added the key to the right user's `~/.ssh/authorized_keys`, not root by default.
- **Multiple keys**: SSH tries keys in order; specify one with `ssh -i ~/.ssh/winmux_key user@host`.
