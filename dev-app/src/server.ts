/**
 * Local HTTPS server for `wavedash dev`.
 *
 * Replaces the CDP `Fetch.enable` interceptor that previously hijacked the
 * game subdomain inside chromium itself. Chromium now reaches us through
 * `--host-rules=MAP *.<localHostSuffix>:443 127.0.0.1:<port>`, so the network
 * stack stays untouched and the bundled DevTools' Network tab works.
 *
 * Routing per request (matches the previous interceptor 1:1):
 *   1. Path `/dev-app-embed`       → synthesize the embed shell (or
 *                                     CUSTOM-HTML with the SDK bootstrap
 *                                     injected).
 *   2. Path in PASSTHROUGH_PREFIXES → reverse-proxy to the real
 *                                     `https://<incoming host>` (Node DNS,
 *                                     no host-rules → real network).
 *   3. Otherwise                    → serve a file from `uploadDir` with
 *                                     COEP/COOP/CORP and transparent
 *                                     gzip/br Content-Encoding.
 */

import * as fs from "node:fs";
import * as http from "node:http";
import * as https from "node:https";
import * as path from "node:path";
import { URL } from "node:url";

import { lookup as mimeLookup } from "mime-types";

import { generateCert, type CertPair } from "./cert";

export interface ServerConfig {
  uploadDir: string;
  localHostSuffix: string;
  verbose: boolean;
}

export interface StartedServer {
  port: number;
  cert: CertPair;
  close: () => Promise<void>;
}

const PASSTHROUGH_PREFIXES = [
  "/embed.js",
  "/embed.css",
  "/sw-embed.js",
  "/default-entrypoints/",
  "/auth/refresh",
  "/sw-bootstrap",
  "/local-embed",
];

// Per-process nonce busts the play worker's immutable embed.js cache between `wavedash dev` runs while still letting in-session reloads hit the disk cache.
const EMBED_BOOTSTRAP_TAG = `<script src="/embed.js?v=local-${Date.now()}"></script>`;

/** Per-request access log (vite/caddy style). serve_local lines are always
 *  on; synth/passthrough are gated behind --verbose at the call sites. */
function log(...args: unknown[]): void {
  process.stderr.write(args.map(String).join(" ") + "\n");
}

/** Local-file access log. Emitted after the response status + headers are
 *  committed, so the line reflects what was actually returned. */
function logServed(res: http.ServerResponse, url: string): void {
  const ts = new Date().toTimeString().slice(0, 8);
  const ct = (res.getHeader("Content-Type") as string | undefined) ?? "-";
  const ce = (res.getHeader("Content-Encoding") as string | undefined) ?? "-";
  process.stderr.write(`${ts}  ${res.statusCode}  ${ct}  ${ce}  ${url}\n`);
}

export async function startServer(
  config: ServerConfig,
): Promise<StartedServer> {
  const cert = generateCert(config.localHostSuffix);

  const server = https.createServer(
    { cert: cert.certPem, key: cert.keyPem },
    (req, res) => {
      void handle(req, res, config);
    },
  );

  await new Promise<void>((resolve, reject) => {
    const onError = (err: Error): void => reject(err);
    server.once("error", onError);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", onError);
      resolve();
    });
  });

  const address = server.address();
  if (!address || typeof address === "string") {
    throw new Error("local server did not bind to an address");
  }

  return {
    port: address.port,
    cert,
    close: () =>
      new Promise<void>((resolve) => {
        server.close(() => resolve());
      }),
  };
}

async function handle(
  req: http.IncomingMessage,
  res: http.ServerResponse,
  config: ServerConfig,
): Promise<void> {
  // SNI is honored by https.createServer, so req.headers.host carries the
  // browser-facing host (the game subdomain). chromium's host-rules MAP
  // always sets it; we reject anything missing rather than guessing.
  const hostHeader = req.headers.host;
  if (!hostHeader) {
    res.statusCode = 400;
    res.end("Missing Host header");
    return;
  }
  // Strip the optional `:port` so the proxy origin and logs use the bare host.
  const originHost = hostHeader.split(":")[0];
  const url = new URL(req.url ?? "/", `https://${hostHeader}`);
  const urlPath = url.pathname;

  try {
    if (isPassthroughPath(urlPath)) {
      if (config.verbose) {
        log(
          "passthrough",
          req.method ?? "GET",
          originHost + urlPath + url.search,
        );
      }
      await proxyToOrigin(req, res, originHost);
      return;
    }

    serveLocalFile(res, config.uploadDir, urlPath);
    logServed(res, originHost + urlPath);
  } catch (err) {
    process.stderr.write(
      `handler error for ${req.url ?? ""}: ${String(err)}\n`,
    );
    if (!res.headersSent) {
      res.statusCode = 500;
      res.setHeader("Content-Type", "text/plain; charset=utf-8");
      setIframeOriginHeaders(res);
      res.end("Internal Server Error");
    } else {
      res.end();
    }
  }
}

function isPassthroughPath(p: string): boolean {
  return PASSTHROUGH_PREFIXES.some((prefix) => p.startsWith(prefix));
}


/**
 * Headers every response on the game subdomain must carry.
 */
function setIframeOriginHeaders(res: http.ServerResponse): void {
  res.setHeader("Origin-Agent-Cluster", "?1");
  res.setHeader("Cross-Origin-Embedder-Policy", "require-corp");
  res.setHeader("Cross-Origin-Opener-Policy", "same-origin");
  res.setHeader("Cross-Origin-Resource-Policy", "cross-origin");
}

function readCustomHtml(uploadDir: string, entrypoint: string): string {
  const relative = entrypoint.replace(/^\/+/, "");
  const filePath = path.join(uploadDir, relative);
  let canonicalDir = uploadDir;
  let canonicalFile = filePath;
  try {
    canonicalDir = fs.realpathSync(uploadDir);
  } catch {
    // uploadDir may not be canonicalizable on some filesystems — fall back.
  }
  try {
    canonicalFile = fs.realpathSync(filePath);
  } catch {
    canonicalFile = filePath;
  }
  if (!isInside(canonicalFile, canonicalDir)) {
    throw new Error("Entrypoint escapes upload_dir");
  }
  return fs.readFileSync(filePath, "utf8");
}

function injectEmbedBootstrap(html: string): string {
  const idx = findHeadOpen(html);
  if (idx !== null) {
    return html.slice(0, idx) + EMBED_BOOTSTRAP_TAG + html.slice(idx);
  }
  return EMBED_BOOTSTRAP_TAG + html;
}

/** Returns the index right after the first `<head ...>` open tag, or null. */
function findHeadOpen(html: string): number | null {
  const lower = html.toLowerCase();
  let searchFrom = 0;
  while (true) {
    const start = lower.indexOf("<head", searchFrom);
    if (start === -1) return null;
    const afterTagName = start + "<head".length;
    if (afterTagName >= html.length) return null;
    const next = html.charCodeAt(afterTagName);
    // Must be `<head>` or `<head ...>` — not `<header>`, `<headline>`, etc.
    const isWhitespace =
      next === 0x09 ||
      next === 0x0a ||
      next === 0x0b ||
      next === 0x0c ||
      next === 0x0d ||
      next === 0x20;
    if (next === 0x3e /* > */ || isWhitespace) {
      const close = lower.indexOf(">", afterTagName);
      if (close === -1) return null;
      return close + 1;
    }
    searchFrom = afterTagName;
  }
}

function serveLocalFile(
  res: http.ServerResponse,
  uploadDir: string,
  urlPath: string,
): void {
  let relative = urlPath.replace(/^\/+/, "");
  try {
    relative = decodeURIComponent(relative);
  } catch {
    // Keep raw if decode fails — matches the old Rust urlencoding fallback.
  }
  const filePath = path.join(uploadDir, relative);

  let canonicalDir = uploadDir;
  let canonicalFile = filePath;
  try {
    canonicalDir = fs.realpathSync(uploadDir);
  } catch {
    // ignore
  }
  try {
    canonicalFile = fs.realpathSync(filePath);
  } catch {
    canonicalFile = filePath;
  }
  if (!isInside(canonicalFile, canonicalDir)) {
    sendNotFound(res);
    return;
  }

  let stat: fs.Stats;
  try {
    stat = fs.statSync(filePath);
  } catch {
    sendNotFound(res);
    return;
  }
  if (!stat.isFile()) {
    sendNotFound(res);
    return;
  }

  const { contentType, contentEncoding } = resolveContentType(urlPath);

  // CUSTOM-HTML iframe path: dev-app serves the developer's HTML from disk
  // and injects the SDK bootstrap, mirroring play/src/server/handlers/embed.tsx
  // for prod builds. Content-Encoding-coded responses (.html.gz) skip
  // injection — we'd have to decompress to mutate, and HTML is rarely
  // pre-compressed in dev builds.
  const isHtml =
    !contentEncoding && contentType.startsWith("text/html");
  if (isHtml) {
    let body: string;
    try {
      body = fs.readFileSync(filePath, "utf8");
    } catch (err) {
      process.stderr.write(`read error for ${urlPath}: ${String(err)}\n`);
      sendNotFound(res);
      return;
    }
    const injected = injectEmbedBootstrap(body);
    res.statusCode = 200;
    res.setHeader("Access-Control-Allow-Origin", "*");
    setIframeOriginHeaders(res);
    // no-store keeps both the browser cache AND the play SW asset cache
    // (which intercepts on the prod-testing flow) from serving stale copies
    // of disk-edited files.
    res.setHeader("Cache-Control", "no-store");
    res.setHeader("Content-Type", contentType);
    res.setHeader("Content-Length", Buffer.byteLength(injected));
    res.end(injected);
    return;
  }

  res.statusCode = 200;
  res.setHeader("Access-Control-Allow-Origin", "*");
  setIframeOriginHeaders(res);
  res.setHeader("Cache-Control", "no-store");
  res.setHeader("Content-Type", contentType);
  if (contentEncoding) {
    res.setHeader("Content-Encoding", contentEncoding);
  }
  res.setHeader("Content-Length", stat.size);

  const stream = fs.createReadStream(filePath);
  stream.on("error", (err) => {
    process.stderr.write(`stream error for ${urlPath}: ${String(err)}\n`);
    res.destroy();
  });
  stream.pipe(res);
}

function sendNotFound(res: http.ServerResponse): void {
  res.statusCode = 404;
  res.setHeader("Content-Type", "text/plain; charset=utf-8");
  setIframeOriginHeaders(res);
  res.end("Not Found");
}

/** Engine-specific MIME overrides. mime-types ships with most web/media
 *  extensions but doesn't know engine-specific bundle formats, and gets
 *  Unity's `.symbols.json` actively wrong (returns application/json — Unity's
 *  loader fetches it as a binary blob and needs octet-stream).
 *
 *  Unity WebGL deploy guide: https://docs.unity3d.com/Manual/webgl-deploying.html
 *  Mirrors play/src/server/utils/mime.ts on the production play worker. */
const ENGINE_MIME: Record<string, string> = {
  // Unity WebGL
  ".unityweb": "application/octet-stream", // legacy decompression-fallback bundle
  ".unity3d": "application/vnd.unity", // legacy unity package
  ".data": "application/octet-stream", // assets / asset bundles
  ".wasm": "application/wasm", // streaming compile requires this exact type
  ".mem": "application/octet-stream", // asm.js memory blob (older Unity)
  ".bundle": "application/octet-stream", // generic asset bundle
  // Godot HTML5
  ".pck": "application/octet-stream", // Godot pack file
};

function fileExtension(p: string): string {
  const lastDot = p.lastIndexOf(".");
  return lastDot === -1 ? "" : p.slice(lastDot).toLowerCase();
}

function lookupContentType(p: string): string {
  // Unity development builds emit <Name>.symbols.json. Unity's loader treats
  // it as opaque bytes, so force octet-stream — mime-types would say JSON.
  if (p.endsWith(".symbols.json")) return "application/octet-stream";

  const override = ENGINE_MIME[fileExtension(p)];
  if (override) return override;

  return mimeLookup(p) || "application/octet-stream";
}

function resolveContentType(urlPath: string): {
  contentType: string;
  contentEncoding: string | null;
} {
  // Unity emits `<file>.gz` / `<file>.br` directly when its compression
  // option is gzip or brotli — strip the suffix to derive the real type and
  // set Content-Encoding so the browser transparently decompresses.
  // Note: `.unityweb` is NOT compression-encoded — Unity's JS decompressor
  // unpacks it client-side, so passing through with no Content-Encoding is
  // correct (already handled by ENGINE_MIME above).
  const compressionMap: Array<[string, string]> = [
    [".gz", "gzip"],
    [".br", "br"],
  ];

  for (const [suffix, encoding] of compressionMap) {
    if (urlPath.endsWith(suffix)) {
      const stripped = urlPath.slice(0, urlPath.length - suffix.length);
      return {
        contentType: lookupContentType(stripped),
        contentEncoding: encoding,
      };
    }
  }
  return { contentType: lookupContentType(urlPath), contentEncoding: null };
}

function isInside(child: string, parent: string): boolean {
  const rel = path.relative(parent, child);
  return !rel.startsWith("..") && !path.isAbsolute(rel);
}

/**
 * Reverse-proxy a request to `https://<originHost><path>`. Node's DNS
 * resolution is unaffected by chromium's `--host-rules`, so the request
 * goes straight to the real production play worker.
 *
 * We strip hop-by-hop headers (RFC 7230 §6.1) and let Node set Host/
 * Content-Length itself; everything else is passed through.
 */
async function proxyToOrigin(
  req: http.IncomingMessage,
  res: http.ServerResponse,
  originHost: string,
): Promise<void> {
  const HOP_BY_HOP = new Set([
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
    "content-length",
  ]);

  const outHeaders: http.OutgoingHttpHeaders = {};
  for (const [k, v] of Object.entries(req.headers)) {
    if (v === undefined) continue;
    if (HOP_BY_HOP.has(k.toLowerCase())) continue;
    outHeaders[k] = v;
  }

  await new Promise<void>((resolve, reject) => {
    const upstream = https.request(
      {
        host: originHost,
        port: 443,
        method: req.method,
        path: req.url,
        headers: outHeaders,
        // Loopback only: lvh.me wildcards resolve to 127.0.0.1, so this hits
        // the user's local Caddy (or whatever dev proxy). Caddy's dev cert
        // isn't in Node's CA bundle (Node ignores the macOS keychain), so we
        // skip verification — there's no MITM surface on localhost.
        rejectUnauthorized: false,
      },
      (upRes) => {
        res.statusCode = upRes.statusCode ?? 502;
        for (const [k, v] of Object.entries(upRes.headers)) {
          if (v === undefined) continue;
          if (HOP_BY_HOP.has(k.toLowerCase())) continue;
          res.setHeader(k, v as string | string[]);
        }
        upRes.pipe(res);
        upRes.on("end", () => resolve());
        upRes.on("error", reject);
      },
    );
    upstream.on("error", (err) => {
      process.stderr.write(
        `proxy error for ${originHost}${req.url ?? ""}: ${String(err)}\n`,
      );
      if (!res.headersSent) {
        res.statusCode = 502;
        res.setHeader("Content-Type", "text/plain; charset=utf-8");
        setIframeOriginHeaders(res);
        res.end("Bad Gateway");
      } else {
        res.destroy();
      }
      reject(err);
    });
    req.pipe(upstream);
  });
}
