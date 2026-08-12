# papier → Kindle

Any doc in the library can be emailed to a Kindle through Amazon's
Send-to-Kindle gateway. Three ways to trigger it:

- **Web viewer** — "⇪ Send to Kindle" in the doc sidebar / docbar at
  `remarkable.exe.xyz/papier/` (POST `/papier/api/kindle`).
- **Mac** — `make kindle DOC=<doc-id> [FORMAT=auto|epub|pdf]` in `papier/`.
- **VM** — `~/bin/papier-kindle.sh <doc-id> [--format ...]` directly.

What gets sent (`--format auto`):

| doc | sent as |
| --- | --- |
| ✦ Compose article (markdown retained in `papier-compose/<job>/work/`) | reflowable **EPUB** via pandoc — math as embedded images, article images inlined, `thumb.png` as cover |
| uploaded / mirrored book with a retained source PDF | the **source PDF**, as-is |
| desk-rendered book (no source) | the **derived PDF** (raster pages + invisible text layer) |

`--format pdf` forces the PDF path even for compose docs. `--format epub`
errors on docs with no markdown source — PDF→EPUB conversion mangles
papers, so it is deliberately not offered. Ink never leaves the
tablet/web reader; the Kindle copy is the clean document. Amazon's email
gateway caps attachments at 50MB; the script refuses larger files.

## One-time setup

### 1. Amazon side

At amazon.com → Account → Content & Devices → Preferences → **Personal
Document Settings**:

- Note the device's **Send-to-Kindle e-mail** (`...@kindle.com`).
- Add the VM's sender address to the **Approved Personal Document E-mail
  List** — this is the `KINDLE_FROM` below; mail from unapproved senders
  is silently dropped.

### 2. VM: SMTP relay (`~/.msmtprc`, mode 600)

msmtp is installed by `deploy-server.sh`. For Gmail, create an app
password (Google Account → Security → 2-Step Verification → App
passwords) and write:

```
defaults
auth           on
tls            on
tls_trust_file /etc/ssl/certs/ca-certificates.crt
logfile        ~/.msmtp.log

account default
host           smtp.gmail.com
port           587
from           you@gmail.com
user           you@gmail.com
password       <app password>
```

`chmod 600 ~/.msmtprc`. Any other SMTP provider works the same way; a
non-default account name goes in `KINDLE_MSMTP_ACCOUNT`.

### 3. VM: `~/.papier-kindle.env`

```
KINDLE_TO=your-address@kindle.com
KINDLE_FROM=you@gmail.com
```

Optional: `KINDLE_MSMTP_ACCOUNT` (msmtp account, default `default`),
`KINDLE_WEBTEX` (pandoc --webtex URL prefix for math images; default
codecogs at 200dpi).

### 4. Verify

```
ssh exedev@remarkable.exe.xyz 'bin/papier-kindle.sh <some-doc-id>'
```

`sent: <file> (<n>KB) -> ...@kindle.com` means it worked; the doc lands
on the Kindle within a minute or two. Failures print one line naming the
missing piece (config, msmtp, pandoc, approved sender).
