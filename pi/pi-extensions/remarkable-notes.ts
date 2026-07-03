/**
 * reMarkable notebook tools for pi, for running ON the tablet itself.
 *
 * Gives pi three tools over xochitl's document storage
 * (/home/root/.local/share/remarkable/xochitl):
 *
 *   remarkable_list   - browse/search documents and folders
 *   remarkable_read   - read a notebook: typed text extracted from .rm v6
 *                       "root text" blocks (best effort), and handwritten
 *                       pages rendered from raw pen strokes into PNGs that
 *                       are attached to the tool result for the model to see
 *   remarkable_write  - create a new document (markdown -> EPUB) and try to
 *                       make xochitl pick it up via its USB web-interface API
 *
 * No dependencies beyond node stdlib: the JSON schemas are plain objects and
 * the EPUB writer emits a STORED (uncompressed) zip by hand.
 */

import * as fs from "node:fs";
import * as path from "node:path";
import * as zlib from "node:zlib";
import { randomUUID } from "node:crypto";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const XOCHITL = "/home/root/.local/share/remarkable/xochitl";
const UPLOAD_URLS = ["http://10.11.99.1:80/upload", "http://127.0.0.1:80/upload"];
// Touching this flag arms pi-rm-refresh.service (installed by pi-appload):
// it restarts the UI to pick up dropped documents as soon as no terminal
// session is open, so nothing the user is doing gets killed.
const REFRESH_FLAG = "/home/root/.local/share/pi-rm-refresh-pending";

function armUiRefresh(): boolean {
  try {
    fs.writeFileSync(REFRESH_FLAG, String(Date.now()));
    return true;
  } catch {
    return false;
  }
}

// ---------------------------------------------------------------------------
// metadata helpers

interface DocMeta {
  id: string;
  visibleName: string;
  type: string; // DocumentType | CollectionType
  parent: string; // "" root, "trash", or folder uuid
  deleted?: boolean;
  lastModified?: string;
  fileType?: string; // notebook | pdf | epub (from .content)
  pageCount?: number;
}

function readJson(p: string): any | null {
  try {
    return JSON.parse(fs.readFileSync(p, "utf8"));
  } catch {
    return null;
  }
}

function loadAll(): Map<string, DocMeta> {
  const docs = new Map<string, DocMeta>();
  for (const f of fs.readdirSync(XOCHITL)) {
    if (!f.endsWith(".metadata")) continue;
    const id = f.slice(0, -".metadata".length);
    const md = readJson(path.join(XOCHITL, f));
    if (!md || md.deleted) continue;
    const doc: DocMeta = {
      id,
      visibleName: md.visibleName ?? id,
      type: md.type ?? "?",
      parent: md.parent ?? "",
      lastModified: md.lastModified,
    };
    const content = readJson(path.join(XOCHITL, `${id}.content`));
    if (content) {
      doc.fileType = content.fileType || "notebook";
      doc.pageCount =
        content.cPages?.pages?.filter((p: any) => !p.deleted)?.length ??
        content.pageCount ??
        content.pages?.length;
    }
    docs.set(id, doc);
  }
  return docs;
}

function fullPath(doc: DocMeta, docs: Map<string, DocMeta>): string {
  const parts = [doc.visibleName];
  let cur = doc.parent;
  let hops = 0;
  while (cur && cur !== "trash" && hops++ < 20) {
    const p = docs.get(cur);
    if (!p) break;
    parts.unshift(p.visibleName);
    cur = p.parent;
  }
  return (doc.parent === "trash" ? "[trash] /" : "/") + parts.join("/");
}

function fmtDate(ms?: string): string {
  if (!ms) return "?";
  const d = new Date(Number(ms));
  return Number.isNaN(d.getTime()) ? "?" : d.toISOString().slice(0, 16).replace("T", " ");
}

function resolveDoc(
  docs: Map<string, DocMeta>,
  idOrName: string,
): { doc?: DocMeta; candidates?: DocMeta[] } {
  if (docs.has(idOrName)) return { doc: docs.get(idOrName) };
  const q = idOrName.toLowerCase();
  const hits = [...docs.values()].filter(
    (d) => d.type === "DocumentType" && d.visibleName.toLowerCase().includes(q),
  );
  if (hits.length === 1) return { doc: hits[0] };
  const exact = hits.filter((d) => d.visibleName.toLowerCase() === q);
  if (exact.length === 1) return { doc: exact[0] };
  return { candidates: hits.slice(0, 10) };
}

// ---------------------------------------------------------------------------
// .rm v6 typed-text extraction (best effort)

const RM_V6_HEADER = "reMarkable .lines file, version=6";

/** Iterate top-level blocks of a v6 .rm file: [u32 len][u8?][u8 min][u8 cur][u8 type] payload */
function* v6Blocks(buf: Buffer): Generator<{ type: number; version: number; payload: Buffer }> {
  // The header magic is followed by space padding; blocks start after it.
  let off = RM_V6_HEADER.length;
  while (off < buf.length && buf[off] === 0x20) off++;
  while (off + 8 <= buf.length) {
    const len = buf.readUInt32LE(off);
    const version = buf[off + 6];
    const type = buf[off + 7];
    const start = off + 8;
    if (len === 0 || start + len > buf.length) break;
    yield { type, version, payload: buf.subarray(start, start + len) };
    off = start + len;
  }
}

/**
 * Pull text runs out of a RootText (0x07) block payload. Text items store
 * their string as [varuint len][u8 isAscii][bytes]; rather than fully decode
 * the CRDT item framing, scan for that shape and keep plausible UTF-8 runs.
 * File order approximates typing order for linearly-written notes.
 */
function extractTextFromRootText(payload: Buffer): string[] {
  const runs: string[] = [];
  for (let i = 0; i < payload.length - 2; i++) {
    // varuint length (1-2 bytes is plenty for note text chunks)
    let len = payload[i];
    let lenBytes = 1;
    if (len >= 0x80) {
      if (i + 1 >= payload.length) continue;
      len = (len & 0x7f) | (payload[i + 1] << 7);
      lenBytes = 2;
      if (payload[i + 1] >= 0x80) continue;
    }
    const flag = payload[i + lenBytes];
    if (flag !== 0x01 || len === 0 || len > 4096) continue;
    const start = i + lenBytes + 1;
    if (start + len > payload.length) continue;
    const slice = payload.subarray(start, start + len);
    const text = slice.toString("utf8");
    // printable check: reject if it contains control chars / replacement chars
    if (/[\u0000-\u0008\u000B-\u001F\u007F\uFFFD]/.test(text)) continue;
    if (len >= 2 || /[\w\s.,!?'"()\-\n]/.test(text)) {
      runs.push(text);
      i = start + len - 1;
    }
  }
  return runs;
}

// ---------------------------------------------------------------------------
// .rm v6 stroke parsing + PNG rendering (handwritten pages -> images the
// model can read; no native deps, PNG via node:zlib)

class Cur {
  b: Buffer;
  o = 0;
  constructor(b: Buffer) {
    this.b = b;
  }
  u8() {
    return this.b[this.o++];
  }
  u16() {
    const v = this.b.readUInt16LE(this.o);
    this.o += 2;
    return v;
  }
  u32() {
    const v = this.b.readUInt32LE(this.o);
    this.o += 4;
    return v;
  }
  f32() {
    const v = this.b.readFloatLE(this.o);
    this.o += 4;
    return v;
  }
  f64() {
    const v = this.b.readDoubleLE(this.o);
    this.o += 8;
    return v;
  }
  varuint() {
    let r = 0,
      s = 0,
      byte;
    do {
      byte = this.u8();
      r |= (byte & 0x7f) << s;
      s += 7;
    } while (byte & 0x80);
    return r >>> 0;
  }
  expect(index: number, type: number) {
    const save = this.o;
    const t = this.varuint();
    if (t >> 4 !== index || (t & 0xf) !== type) {
      this.o = save;
      throw new Error("tag mismatch");
    }
  }
  crdtId(index: number) {
    this.expect(index, 0xf);
    this.u8();
    this.varuint();
  }
  taggedU32(index: number) {
    this.expect(index, 0x4);
    return this.u32();
  }
  taggedF32(index: number) {
    this.expect(index, 0x4);
    return this.f32();
  }
  taggedF64(index: number) {
    this.expect(index, 0x8);
    return this.f64();
  }
  subblock(index: number) {
    this.expect(index, 0xc);
    return this.u32();
  }
}

interface Stroke {
  color: number;
  pts: Array<{ x: number; y: number; w: number }>;
}

/** Parse SceneLineItemBlocks (0x05) into strokes (see rmscene for the spec). */
function parseV6Strokes(buf: Buffer): Stroke[] {
  const strokes: Stroke[] = [];
  for (const blk of v6Blocks(buf)) {
    if (blk.type !== 0x05) continue;
    try {
      const c = new Cur(blk.payload);
      c.crdtId(1); // parent
      c.crdtId(2); // item
      c.crdtId(3); // left
      c.crdtId(4); // right
      c.taggedU32(5); // deleted_length
      try {
        c.subblock(6);
      } catch {
        continue; // tombstone item: no value
      }
      if (c.u8() !== 0x03) continue; // not a Line
      c.taggedU32(1); // tool
      const color = c.taggedU32(2);
      c.taggedF64(3); // thickness_scale
      c.taggedF32(4); // starting_length
      const ptLen = c.subblock(5);
      const ptSize = blk.version >= 2 ? 14 : 24;
      const n = Math.floor(ptLen / ptSize);
      const pts: Stroke["pts"] = [];
      for (let i = 0; i < n; i++) {
        const x = c.f32();
        const y = c.f32();
        let w: number;
        if (blk.version >= 2) {
          c.u16(); // speed
          w = c.u16();
          c.u8(); // direction
          c.u8(); // pressure
        } else {
          c.f32();
          c.f32();
          w = Math.round(c.f32() * 4);
          c.f32();
        }
        pts.push({ x, y, w });
      }
      if (pts.length) strokes.push({ color, pts });
    } catch {
      /* skip malformed blocks */
    }
  }
  return strokes;
}

function pngChunk(type: string, data: Buffer): Buffer {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
}

/** Render strokes to an 8-bit grayscale PNG at `scale` of device pixels. */
function renderStrokesPng(strokes: Stroke[], scale = 0.5): Buffer {
  const W = Math.round(1404 * scale);
  let maxY = 1872;
  for (const s of strokes) for (const p of s.pts) maxY = Math.max(maxY, p.y + 40);
  const H = Math.round(Math.min(maxY, 4000) * scale);
  const img = Buffer.alloc(W * H, 255);
  const stamp = (cx: number, cy: number, r: number, val: number) => {
    const x0 = Math.max(0, Math.floor(cx - r));
    const x1 = Math.min(W - 1, Math.ceil(cx + r));
    const y0 = Math.max(0, Math.floor(cy - r));
    const y1 = Math.min(H - 1, Math.ceil(cy + r));
    for (let y = y0; y <= y1; y++)
      for (let x = x0; x <= x1; x++)
        if ((x - cx) ** 2 + (y - cy) ** 2 <= r * r && img[y * W + x] > val) img[y * W + x] = val;
  };
  for (const s of strokes) {
    if (s.color === 2) continue; // white "ink"
    const val = s.color === 1 ? 128 : 0; // gray or black
    let prev: { x: number; y: number } | null = null;
    for (const p of s.pts) {
      const x = (p.x + 702) * scale;
      const y = p.y * scale;
      const r = Math.max(0.8, ((p.w / 4) * scale) / 2);
      if (prev) {
        const d = Math.hypot(x - prev.x, y - prev.y);
        const steps = Math.max(1, Math.ceil(d / Math.max(1, r * 0.7)));
        for (let i = 1; i <= steps; i++)
          stamp(prev.x + ((x - prev.x) * i) / steps, prev.y + ((y - prev.y) * i) / steps, r, val);
      } else {
        stamp(x, y, r, val);
      }
      prev = { x, y };
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(W, 0);
  ihdr.writeUInt32BE(H, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 0; // grayscale
  const raw = Buffer.alloc(H * (W + 1));
  for (let y = 0; y < H; y++) {
    raw[y * (W + 1)] = 0; // filter: none
    img.copy(raw, y * (W + 1) + 1, y * W, (y + 1) * W);
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    pngChunk("IHDR", ihdr),
    pngChunk("IDAT", zlib.deflateSync(raw, { level: 6 })),
    pngChunk("IEND", Buffer.alloc(0)),
  ]);
}

// ---------------------------------------------------------------------------
// markdown -> EPUB (stored zip, no deps)

const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

function crc32(buf: Buffer): number {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

/** Minimal STORED zip: good enough for EPUB (mimetype must be stored anyway). */
function makeZip(entries: Array<[string, Buffer | string]>): Buffer {
  const locals: Buffer[] = [];
  const centrals: Buffer[] = [];
  let offset = 0;
  for (const [name, data] of entries) {
    const nameB = Buffer.from(name, "utf8");
    const dataB = Buffer.isBuffer(data) ? data : Buffer.from(data, "utf8");
    const crc = crc32(dataB);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4); // version needed
    local.writeUInt16LE(0, 6); // flags
    local.writeUInt16LE(0, 8); // method: stored
    local.writeUInt32LE(0, 10); // dos time/date
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(dataB.length, 18);
    local.writeUInt32LE(dataB.length, 22);
    local.writeUInt16LE(nameB.length, 26);
    local.writeUInt16LE(0, 28);
    locals.push(local, nameB, dataB);

    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE(20, 4);
    central.writeUInt16LE(20, 6);
    central.writeUInt16LE(0, 8);
    central.writeUInt16LE(0, 10);
    central.writeUInt32LE(0, 12);
    central.writeUInt32LE(crc, 16);
    central.writeUInt32LE(dataB.length, 20);
    central.writeUInt32LE(dataB.length, 24);
    central.writeUInt16LE(nameB.length, 28);
    central.writeUInt32LE(0, 30); // extra/comment/disk/attrs(int)
    central.writeUInt32LE(0, 34); // disk start / internal attrs
    central.writeUInt32LE(0, 38); // external attrs
    central.writeUInt32LE(offset, 42);
    centrals.push(central, nameB);
    offset += local.length + nameB.length + dataB.length;
  }
  const centralStart = offset;
  const centralBuf = Buffer.concat(centrals);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(centralBuf.length, 12);
  end.writeUInt32LE(centralStart, 16);
  return Buffer.concat([...locals, centralBuf, end]);
}

function esc(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/** Small markdown subset: #/##/### headings, ``` code, - lists, paragraphs. */
function mdToXhtml(md: string): string {
  const out: string[] = [];
  const lines = md.split("\n");
  let inCode = false;
  let inList = false;
  const closeList = () => {
    if (inList) out.push("</ul>");
    inList = false;
  };
  for (const line of lines) {
    if (line.trim().startsWith("```")) {
      closeList();
      out.push(inCode ? "</pre>" : "<pre>");
      inCode = !inCode;
      continue;
    }
    if (inCode) {
      out.push(esc(line));
      continue;
    }
    const h = line.match(/^(#{1,3})\s+(.*)/);
    if (h) {
      closeList();
      const n = h[1].length;
      out.push(`<h${n}>${esc(h[2])}</h${n}>`);
      continue;
    }
    const li = line.match(/^\s*[-*]\s+(.*)/);
    if (li) {
      if (!inList) out.push("<ul>");
      inList = true;
      out.push(`<li>${esc(li[1])}</li>`);
      continue;
    }
    closeList();
    if (line.trim() === "") continue;
    out.push(`<p>${esc(line)}</p>`);
  }
  if (inCode) out.push("</pre>");
  closeList();
  return out.join("\n");
}

function makeEpub(title: string, markdown: string): Buffer {
  const body = mdToXhtml(markdown);
  const uuid = randomUUID();
  const xhtml = `<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>${esc(title)}</title></head>
<body>${body}</body></html>`;
  const opf = `<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="id">urn:uuid:${uuid}</dc:identifier>
    <dc:title>${esc(title)}</dc:title>
    <dc:language>en</dc:language>
    <meta property="dcterms:modified">${new Date().toISOString().slice(0, 19)}Z</meta>
  </metadata>
  <manifest><item id="doc" href="doc.xhtml" media-type="application/xhtml+xml"/></manifest>
  <spine><itemref idref="doc"/></spine>
</package>`;
  const container = `<?xml version="1.0" encoding="utf-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>`;
  return makeZip([
    ["mimetype", "application/epub+zip"],
    ["META-INF/container.xml", container],
    ["content.opf", opf],
    ["doc.xhtml", xhtml],
  ]);
}

// ---------------------------------------------------------------------------
// .rm v6 writing: a notebook page with editable TYPED text (root text block),
// mirroring rmscene's simple_text_document block sequence.

class Wr {
  a: number[] = [];
  u8(v: number) {
    this.a.push(v & 0xff);
  }
  u16(v: number) {
    this.u8(v);
    this.u8(v >> 8);
  }
  u32(v: number) {
    this.u8(v);
    this.u8(v >> 8);
    this.u8(v >> 16);
    this.u8(v >> 24);
  }
  f32(v: number) {
    const b = Buffer.alloc(4);
    b.writeFloatLE(v);
    this.bytes(b);
  }
  f64(v: number) {
    const b = Buffer.alloc(8);
    b.writeDoubleLE(v);
    this.bytes(b);
  }
  bytes(b: Buffer | Uint8Array) {
    for (const x of b) this.a.push(x);
  }
  varuint(v: number) {
    do {
      let byte = v & 0x7f;
      v >>>= 7;
      if (v) byte |= 0x80;
      this.u8(byte);
    } while (v);
  }
  tag(index: number, type: number) {
    this.varuint((index << 4) | type);
  }
  id(index: number, p1: number, p2: number) {
    this.tag(index, 0xf);
    this.u8(p1);
    this.varuint(p2);
  }
  idRaw(p1: number, p2: number) {
    this.u8(p1);
    this.varuint(p2);
  }
  bool(index: number, v: boolean) {
    this.tag(index, 0x1);
    this.u8(v ? 1 : 0);
  }
  int(index: number, v: number) {
    this.tag(index, 0x4);
    this.u32(v);
  }
  float(index: number, v: number) {
    this.tag(index, 0x4);
    this.f32(v);
  }
  sub(index: number, fn: (w: Wr) => void) {
    const w = new Wr();
    fn(w);
    this.tag(index, 0xc);
    this.u32(w.a.length);
    this.a.push(...w.a);
  }
  str(index: number, s: string) {
    this.sub(index, (w) => {
      const b = Buffer.from(s, "utf8");
      w.varuint(b.length);
      w.u8(1);
      w.bytes(b);
    });
  }
  lwwStr(index: number, ts2: number, s: string) {
    this.sub(index, (w) => {
      w.id(1, 0, ts2);
      w.str(2, s);
    });
  }
  lwwBool(index: number, ts2: number, v: boolean) {
    this.sub(index, (w) => {
      w.id(1, 0, ts2);
      w.bool(2, v);
    });
  }
  buf() {
    return Buffer.from(this.a);
  }
}

function v6Block(type: number, minVer: number, curVer: number, fn: (w: Wr) => void): Buffer {
  const w = new Wr();
  fn(w);
  const h = new Wr();
  h.u32(w.a.length);
  h.u8(0);
  h.u8(minVer);
  h.u8(curVer);
  h.u8(type);
  return Buffer.concat([h.buf(), w.buf()]);
}

/** Build a complete .rm v6 file whose page contains `text` as typed text. */
function makeTextRm(text: string, authorUuid: string): Buffer {
  const author = Buffer.from(authorUuid.replace(/-/g, ""), "hex");
  // uuid bytes_le: first three fields byte-swapped
  const authorLe = Buffer.concat([
    Buffer.from([author[3], author[2], author[1], author[0]]),
    Buffer.from([author[5], author[4]]),
    Buffer.from([author[7], author[6]]),
    author.subarray(8),
  ]);
  const nLines = text.split("\n").length;
  const parts = [Buffer.from("reMarkable .lines file, version=6".padEnd(43, " "), "latin1")];
  parts.push(
    v6Block(0x09, 1, 1, (w) => {
      w.varuint(1);
      w.sub(0, (s) => {
        s.varuint(16);
        s.bytes(authorLe);
        s.u16(1);
      });
    }),
    v6Block(0x00, 1, 1, (w) => {
      w.id(1, 1, 1);
      w.bool(2, true);
      w.bool(3, false);
    }),
    v6Block(0x0a, 0, 1, (w) => {
      w.int(1, 1);
      w.int(2, 0);
      w.int(3, text.length + 1);
      w.int(4, nLines);
      w.int(5, 0);
    }),
    v6Block(0x01, 1, 1, (w) => {
      w.id(1, 0, 11);
      w.id(2, 0, 0);
      w.bool(3, true);
      w.sub(4, (s) => s.id(1, 0, 1));
    }),
    v6Block(0x07, 1, 1, (w) => {
      w.id(1, 0, 0);
      w.sub(2, (s) => {
        s.sub(1, (a) =>
          a.sub(1, (b) => {
            b.varuint(1);
            b.sub(0, (item) => {
              item.id(2, 1, 16);
              item.id(3, 0, 0);
              item.id(4, 0, 0);
              item.int(5, 0);
              item.str(6, text);
            });
          }),
        );
        s.sub(2, (a) =>
          a.sub(1, (b) => {
            b.varuint(1);
            b.idRaw(0, 0);
            b.id(1, 1, 15);
            b.sub(2, (c) => {
              c.u8(17);
              c.u8(1); // ParagraphStyle.PLAIN
            });
          }),
        );
      });
      w.sub(3, (s) => {
        s.f64(-468.0);
        s.f64(234.0);
      });
      w.float(4, 936.0);
    }),
    v6Block(0x02, 1, 2, (w) => {
      w.id(1, 0, 1);
      w.lwwStr(2, 0, "");
      w.lwwBool(3, 0, true);
    }),
    v6Block(0x02, 1, 2, (w) => {
      w.id(1, 0, 11);
      w.lwwStr(2, 12, "Layer 1");
      w.lwwBool(3, 0, true);
    }),
    v6Block(0x04, 1, 1, (w) => {
      w.id(1, 0, 1);
      w.id(2, 0, 13);
      w.id(3, 0, 0);
      w.id(4, 0, 0);
      w.int(5, 0);
      w.sub(6, (s) => {
        s.u8(0x02);
        s.id(2, 0, 11);
      });
    }),
  );
  return Buffer.concat(parts);
}

function dropTextNotebook(title: string, text: string, parent: string): string {
  const id = randomUUID();
  const pageId = randomUUID();
  const authorUuid = randomUUID();
  const rm = makeTextRm(text, authorUuid);
  fs.mkdirSync(path.join(XOCHITL, id), { recursive: true });
  fs.writeFileSync(path.join(XOCHITL, id, `${pageId}.rm`), rm);
  fs.writeFileSync(
    path.join(XOCHITL, `${id}.content`),
    JSON.stringify(
      {
        cPages: {
          lastOpened: { timestamp: "1:1", value: pageId },
          original: { timestamp: "0:0", value: -1 },
          pages: [
            {
              id: pageId,
              idx: { timestamp: "1:1", value: "ba" },
              template: { timestamp: "1:1", value: "Blank" },
            },
          ],
          uuids: [{ first: authorUuid, second: 1 }],
        },
        coverPageNumber: -1,
        dummyDocument: false,
        extraMetadata: {},
        fileType: "notebook",
        fontName: "",
        formatVersion: 2,
        lineHeight: -1,
        margins: 125,
        orientation: "portrait",
        pageCount: 1,
        pageTags: [],
        sizeInBytes: String(rm.length),
        tags: [],
        textAlignment: "justify",
        textScale: 1,
        zoomMode: "bestFit",
      },
      null,
      2,
    ),
  );
  fs.writeFileSync(path.join(XOCHITL, `${id}.pagedata`), "Blank\n");
  fs.writeFileSync(
    path.join(XOCHITL, `${id}.metadata`),
    JSON.stringify(
      {
        visibleName: title,
        type: "DocumentType",
        parent,
        deleted: false,
        lastModified: String(Date.now()),
        lastOpened: "0",
        lastOpenedPage: 0,
        metadatamodified: false,
        modified: false,
        pinned: false,
        synced: false,
        version: 0,
      },
      null,
      2,
    ),
  );
  return id;
}

async function tryWebUpload(filename: string, epub: Buffer): Promise<string | null> {
  for (const url of UPLOAD_URLS) {
    try {
      const form = new FormData();
      form.append("file", new Blob([epub], { type: "application/epub+zip" }), filename);
      const res = await fetch(url, { method: "POST", body: form, signal: AbortSignal.timeout(8000) });
      if (res.ok) return url;
    } catch {
      /* interface disabled or unreachable; try next */
    }
  }
  return null;
}

function directDrop(title: string, epub: Buffer, parent: string): string {
  const id = randomUUID();
  fs.writeFileSync(path.join(XOCHITL, `${id}.epub`), epub);
  fs.writeFileSync(
    path.join(XOCHITL, `${id}.metadata`),
    JSON.stringify(
      {
        visibleName: title,
        type: "DocumentType",
        parent,
        deleted: false,
        lastModified: String(Date.now()),
        lastOpened: "0",
        lastOpenedPage: 0,
        metadatamodified: false,
        modified: false,
        pinned: false,
        synced: false,
        version: 0,
      },
      null,
      2,
    ),
  );
  fs.writeFileSync(
    path.join(XOCHITL, `${id}.content`),
    JSON.stringify({ fileType: "epub", coverPageNumber: 0 }, null, 2),
  );
  return id;
}

// ---------------------------------------------------------------------------

const REMARKABLE_PROMPT = `

## Environment: reMarkable 2 e-ink tablet

You are running ON a reMarkable 2 (armv7 Linux, 1 GB RAM, busybox userland,
e-ink display). Adjust accordingly:

- For anything involving the user's notes, notebooks, quick sheets, books or
  documents on this device, ALWAYS use the remarkable_list / remarkable_read /
  remarkable_write tools. Do NOT explore
  /home/root/.local/share/remarkable/xochitl with bash: documents there are
  stored as uuid-named files in reMarkable's proprietary formats (.rm v6
  binary strokes + json sidecars) that the tools already decode/encode.
- remarkable_read returns handwritten pages as rendered PNG images in the
  tool result - you can SEE and read the handwriting directly. Use it when
  the user asks about anything they wrote by hand.
- remarkable_write creates notes/documents. When the user asks you to "add
  a note" or save text for them, use the default 'text-notebook' format: it
  produces a real notebook page of EDITABLE TYPED TEXT (like they typed it
  themselves). Only use 'epub' for longer read-only material.
- NEVER run 'systemctl restart xochitl', kill xochitl, or reboot: this
  terminal session runs inside xochitl and would be killed instantly.
- busybox coreutils only (no GNU flags like 'head -n5' shorthand 'head -5',
  no perl); git is not installed; node is at /home/root/opt/node/bin/node.
- RAM is tight (1 GB shared with the UI): avoid heavyweight one-off
  processes; prefer the provided tools.
- The display is e-ink: your output is being read on paper-like refresh, so
  prefer concise output over long scrolling dumps.`;

export default function (pi: ExtensionAPI) {
  // Make pi aware it lives on the tablet and steer it to the tools above.
  pi.on("before_agent_start", async (event: any) => {
    return { systemPrompt: event.systemPrompt + REMARKABLE_PROMPT };
  });

  // E-ink: the default animated spinner + tick counter redraw several times a
  // second, which flickers badly on this panel. Use a static indicator.
  pi.on("session_start", async (_event: any, ctx: any) => {
    try {
      ctx.ui?.setWorkingIndicator?.({ frames: ["◆ working - esc interrupts"], intervalMs: 3600000 });
    } catch {
      /* headless modes have no UI */
    }
  });
  pi.registerTool({
    name: "remarkable_list",
    label: "reMarkable: list documents",
    description:
      "List/search documents and folders in the reMarkable's own storage (xochitl). " +
      "Returns name paths, uuids, types and page counts. Use query to filter by name.",
    parameters: {
      type: "object",
      properties: {
        query: { type: "string", description: "Case-insensitive name filter" },
        limit: { type: "number", description: "Max entries to return (default 50)" },
        folders: { type: "boolean", description: "Include folders (default true)" },
      },
    },
    async execute(_id: string, params: any) {
      const docs = loadAll();
      const q = (params.query ?? "").toLowerCase();
      const limit = params.limit ?? 50;
      const includeFolders = params.folders !== false;
      const rows = [...docs.values()]
        .filter((d) => includeFolders || d.type === "DocumentType")
        .filter((d) => !q || d.visibleName.toLowerCase().includes(q))
        .sort((a, b) => Number(b.lastModified ?? 0) - Number(a.lastModified ?? 0));
      const shown = rows.slice(0, limit);
      const lines = shown.map((d) => {
        const kind = d.type === "CollectionType" ? "folder" : (d.fileType ?? "doc");
        const pages = d.pageCount != null ? ` pages=${d.pageCount}` : "";
        return `${fullPath(d, docs)}  [${kind}]${pages}  modified=${fmtDate(d.lastModified)}  id=${d.id}`;
      });
      const more = rows.length > shown.length ? `\n... and ${rows.length - shown.length} more (raise limit or refine query)` : "";
      return {
        content: [{ type: "text", text: lines.join("\n") + more || "no matches" }],
      };
    },
  });

  pi.registerTool({
    name: "remarkable_read",
    label: "reMarkable: read notebook",
    description:
      "Read a reMarkable notebook: extracts TYPED text (best effort, .rm v6), and " +
      "renders HANDWRITTEN pages as PNG images attached to the result so you can " +
      "read the handwriting yourself. Pass a document uuid or a (partial) name. " +
      "Use firstPage/maxPages to window large notebooks (default: first 4 pages " +
      "with content).",
    parameters: {
      type: "object",
      properties: {
        document: { type: "string", description: "Document uuid or (partial) visible name" },
        firstPage: { type: "number", description: "1-based page to start from (default 1)" },
        maxPages: { type: "number", description: "Max pages to return (default 4, max 8)" },
      },
      required: ["document"],
    },
    async execute(_id: string, params: any) {
      const docs = loadAll();
      const { doc, candidates } = resolveDoc(docs, params.document);
      if (!doc) {
        const c = (candidates ?? []).map((d) => `${fullPath(d, docs)}  id=${d.id}`).join("\n");
        return {
          content: [{ type: "text", text: candidates?.length ? `Ambiguous or not found. Candidates:\n${c}` : "No matching document." }],
          isError: true,
        };
      }
      const content = readJson(path.join(XOCHITL, `${doc.id}.content`));
      if (doc.fileType && doc.fileType !== "notebook") {
        return {
          content: [{
            type: "text",
            text:
              `"${doc.visibleName}" is a ${doc.fileType}; the original file is at ` +
              `${path.join(XOCHITL, doc.id + "." + doc.fileType)}. Page reading only works for notebooks.`,
          }],
        };
      }

      const pageIds: string[] =
        content?.cPages?.pages?.filter((p: any) => !p.deleted)?.map((p: any) => p.id) ??
        content?.pages ??
        [];
      const first = Math.max(1, params.firstPage ?? 1);
      const maxPages = Math.min(params.maxPages ?? 4, 8);
      const out: any[] = [];
      let included = 0;
      let skippedEmpty = 0;

      for (let idx = first - 1; idx < pageIds.length && included < maxPages; idx++) {
        const rmPath = path.join(XOCHITL, doc.id, `${pageIds[idx]}.rm`);
        if (!fs.existsSync(rmPath)) {
          skippedEmpty++;
          continue;
        }
        const buf = fs.readFileSync(rmPath);
        if (!buf.subarray(0, RM_V6_HEADER.length).toString("latin1").startsWith(RM_V6_HEADER)) {
          out.push({ type: "text", text: `--- page ${idx + 1}: unsupported .rm version (not v6) ---` });
          included++;
          continue;
        }
        const texts: string[] = [];
        for (const block of v6Blocks(buf)) {
          if (block.type === 0x07) texts.push(...extractTextFromRootText(block.payload));
        }
        const typed = texts.join("").trim();
        const strokes = parseV6Strokes(buf);
        if (!typed && strokes.length === 0) {
          skippedEmpty++;
          continue;
        }
        out.push({
          type: "text",
          text: `--- page ${idx + 1}${strokes.length ? ` (${strokes.length} pen strokes, rendered below)` : ""} ---${typed ? `\n${typed}` : ""}`,
        });
        if (strokes.length) {
          out.push({
            type: "image",
            data: renderStrokesPng(strokes).toString("base64"),
            mimeType: "image/png",
          });
        }
        included++;
      }

      if (out.length === 0) {
        return {
          content: [{ type: "text", text: `"${doc.visibleName}": no content found in the requested page range (${pageIds.length} pages total).` }],
        };
      }
      const summary =
        `"${doc.visibleName}" (${pageIds.length} pages total, showing from page ${first}` +
        `${skippedEmpty ? `, ${skippedEmpty} empty pages skipped` : ""})`;
      return { content: [{ type: "text", text: summary }, ...out] };
    },
  });

  pi.registerTool({
    name: "remarkable_write",
    label: "reMarkable: write note",
    description:
      "Create a new document on the reMarkable. Two formats:\n" +
      "- 'text-notebook' (DEFAULT - use this for notes): a real notebook page with " +
      "EDITABLE TYPED TEXT, exactly as if the user typed it on the tablet. Plain text " +
      "only (no markdown rendering). Appears right after the pi terminal is closed.\n" +
      "- 'epub': a reflowable read-only document rendered from markdown - use for " +
      "longer reading material. Can appear instantly via the USB web-interface API.",
    parameters: {
      type: "object",
      properties: {
        title: { type: "string", description: "Document title" },
        content: {
          type: "string",
          description:
            "The note content. For text-notebook: plain text (line breaks preserved). " +
            "For epub: markdown (#/##/### headings, - lists, ``` code blocks).",
        },
        format: {
          type: "string",
          enum: ["text-notebook", "epub"],
          description: "Document type (default text-notebook)",
        },
      },
      required: ["title", "content"],
    },
    async execute(_id: string, params: any) {
      const format = params.format ?? "text-notebook";
      const armed = () => armUiRefresh();
      const appearNote = (ok: boolean) =>
        ok
          ? "It will appear automatically right after the user closes this pi terminal " +
            "(the UI reloads itself; nothing is lost). Tell the user that."
          : "The document is on disk but xochitl only scans at startup: it appears after " +
            "the next reboot or 'systemctl restart xochitl' over SSH (NEVER from inside " +
            "this terminal - it would kill this session).";

      if (format === "text-notebook") {
        const id = dropTextNotebook(params.title, params.content, "");
        return {
          content: [{
            type: "text",
            text: `Created notebook "${params.title}" with editable typed text (id=${id}). ${appearNote(armed())}`,
          }],
          details: { id, format },
        };
      }

      const epub = makeEpub(params.title, params.content);
      const filename = `${params.title.replace(/[^\w\- ]+/g, "").trim() || "note"}.epub`;
      const via = await tryWebUpload(filename, epub);
      if (via) {
        return {
          content: [{ type: "text", text: `Uploaded "${params.title}" via ${via} - it should appear in the root folder right away.` }],
        };
      }
      const id = directDrop(params.title, epub, "");
      return {
        content: [{ type: "text", text: `USB web-interface upload not available; wrote the EPUB directly to storage (id=${id}). ${appearNote(armed())}` }],
        details: { id, format },
      };
    },
  });
}
