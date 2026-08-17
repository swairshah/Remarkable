# papier → Kindle

Any doc in the library can be emailed to a Kindle through Amazon's
Send-to-Kindle gateway. Three ways to trigger it:

- **Web viewer** — "⇪ Send to Kindle" in the doc sidebar / docbar at
  `remarkable.exe.xyz/papier/` opens a PDF/EPUB picker. PDF is selected by
  default (POST `/papier/api/kindle`).
- **Mac** — `make kindle DOC=<doc-id> [FORMAT=auto|epub|pdf]` in `papier/`.
- **VM** — `~/bin/papier-kindle.sh <doc-id> [--format ...]` directly.

What gets sent (`--format auto`):

| doc | sent as |
| --- | --- |
| ✦ Compose article (markdown retained in `papier-compose/<job>/work/`) | reflowable **EPUB 3** via pandoc — MathML equations, article images inlined, Reader fonts embedded, `thumb.png` as cover |
| uploaded / mirrored book with a retained source PDF | the **source PDF** with a title sheet prepended to the emailed copy |
| desk-rendered book (no source) | the **derived PDF** (raster pages + invisible text layer), also with the title sheet |

`--format pdf` forces the PDF path even for compose docs. `--format epub`
errors on docs with no markdown source — PDF→EPUB conversion mangles
papers, so it is deliberately not offered. The title sheet and conversion
are outgoing-only: the retained source PDF is never changed. Ink never
leaves the tablet/web reader; the Kindle copy is the clean document.

Resend caps an email at 40MB after attachment Base64 encoding; the
script checks that encoded size before sending.

## One-time setup

### 1. Amazon side

At amazon.com → Account → Content & Devices → Preferences → **Personal
Document Settings**:

- Note the device's **Send-to-Kindle e-mail** (`...@kindle.com`).
- Add the VM's sender address to the **Approved Personal Document E-mail
  List** — this is the `KINDLE_FROM` below; mail from unapproved senders
  is silently dropped.

### 2. Resend side

Create a Resend API key and verify the domain used by your sender. The
`KINDLE_FROM` address does not need an inbox, but its domain must be
verified by Resend and the full address must also appear in Amazon's
Approved Personal Document E-mail List.

### 3. VM: `~/.papier-kindle.env`

```
RESEND_API_KEY=re_...
KINDLE_TO=your-address@kindle.com
KINDLE_FROM='Papier <kindle@your-verified-domain.example>'
```

Protect the credentials with `chmod 600 ~/.papier-kindle.env`. The API
key may instead live in the VM's existing `~/.env` file.

### 4. Verify

```
ssh exedev@remarkable.exe.xyz 'bin/papier-kindle.sh <some-doc-id>'
```

`sent: <file> (<n>KB) -> ...@kindle.com (Resend <email-id>)` means
Resend accepted the message; the document should then arrive through
Amazon's Send-to-Kindle gateway. Failures print one line naming the
missing configuration, Resend API error, Pandoc error, or sender-approval
problem.
