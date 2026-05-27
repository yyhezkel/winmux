# Ручная настройка SSH-ключа

Используйте это руководство, если предпочитаете настроить вход по SSH-ключу вручную, или если автоматическая настройка не сработала.

## Шаг 1 — Сгенерируйте ключ на вашем компьютере

В Windows (PowerShell):
```powershell
ssh-keygen -t ed25519 -f $HOME\.ssh\winmux_key -C "winmux-<server-name>"
```

В macOS или Linux:
```bash
ssh-keygen -t ed25519 -f ~/.ssh/winmux_key -C "winmux-<server-name>"
```

Когда будет запрошена passphrase, нажмите Enter, чтобы оставить пустой, или задайте свою для дополнительной защиты.

## Шаг 2 — Скопируйте ваш публичный ключ

В Windows (PowerShell):
```powershell
Get-Content $HOME\.ssh\winmux_key.pub | Set-Clipboard
```

В macOS:
```bash
pbcopy < ~/.ssh/winmux_key.pub
```

В Linux:
```bash
cat ~/.ssh/winmux_key.pub
```

## Шаг 3 — Установите ключ на сервере

Подключитесь к серверу любым работающим способом (SSH по паролю, web-консоль, терминал облачного провайдера) и выполните:

```bash
mkdir -p ~/.ssh
chmod 700 ~/.ssh
echo "PASTE_YOUR_PUBLIC_KEY_HERE" >> ~/.ssh/authorized_keys
chmod 600 ~/.ssh/authorized_keys
```

Замените `PASTE_YOUR_PUBLIC_KEY_HERE` публичным ключом из шага 2. Это одна строка, начинающаяся с `ssh-ed25519`.

Если есть `ssh-copy-id`, шаги 2 и 3 можно объединить:
```bash
ssh-copy-id -i ~/.ssh/winmux_key.pub user@server.example.com
```

## Шаг 4 — Настройте winmux

Откройте настройки рабочей области и укажите путь к SSH-ключу — `~/.ssh/winmux_key` (или туда, куда вы сохранили приватный ключ).

## Устранение неполадок

- **Permission denied (publickey)**: проверьте права на файлы — `~/.ssh` должен быть 700, `authorized_keys` — 600, приватный ключ — 600.
- **Не тот пользователь на сервере**: убедитесь, что добавили ключ в `~/.ssh/authorized_keys` нужного пользователя, а не root по умолчанию.
- **Несколько ключей**: SSH перебирает ключи по очереди; укажите конкретный с помощью `ssh -i ~/.ssh/winmux_key user@host`.
