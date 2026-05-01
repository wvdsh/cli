/**
 * Wavedash Dev — Electron host for `wavedash dev`.
 *
 * Lifecycle:
 *   1. CLI spawns this binary with `--user-data-dir=<path>` as a CLI arg
 *      and writes one JSON config line to stdin.
 *   2. The user-data-dir MUST be set synchronously at module load — Electron
 *      caches the path the first time it readies internally, and any
 *      `setPath('userData', ...)` after that is a no-op. Reading it from
 *      argv keeps the call sync; the rest of the config waits on stdin.
 *   3. Start a local HTTPS server (`server.ts`) on a free port and apply
 *      `--host-rules=MAP <gameSubdomain>:443 127.0.0.1:<port>` so chromium
 *      routes the game iframe to us. No CDP `Fetch.enable`, so the bundled
 *      DevTools' Network tab works normally.
 *   4. Open a single BrowserWindow, install a cert-verify proc that accepts
 *      our self-signed cert for the game subdomain, and navigate to the
 *      playtest URL.
 *   5. Emit `{"type":"ready"}` on first did-finish-load.
 *   6. On window close → emit `{"type":"closed"}` and exit 0.
 *   7. On stdin EOF → quit (lets the CLI shut us down by closing stdin).
 */

import { join } from "node:path";

// Silence the dev-only "Insecure Content-Security-Policy" / "unsafe-eval"
// console nag. The CSP belongs to wavedash.com (we just navigate to it),
// not to this Electron app, so there's nothing to fix on our side. Electron
// suppresses the warning automatically in packaged builds — setting this
// env var aligns dev mode with prod. Must be set before any electron import
// reads process.env.
process.env.ELECTRON_DISABLE_SECURITY_WARNINGS = "true";

import {
  app,
  BrowserWindow,
  Menu,
  nativeImage,
  session,
  type Session,
} from "electron";

import { startServer, type StartedServer } from "./server";

interface RawConfig {
  uploadDir: string;
  gameSubdomain: string;
  playtestUrl: string;
  verbose?: boolean;
}

const USER_DATA_DIR_FLAG = "--user-data-dir=";

function findUserDataDirArg(): string {
  // Search every argv slot — works whether we're running compiled
  // (argv[0] = exe, argv[1+] = our flags) or via `npx electron .`
  // (argv[0] = electron, argv[1] = '.', argv[2+] = our flags).
  const found = process.argv.find((a) => a.startsWith(USER_DATA_DIR_FLAG));
  if (!found) {
    process.stderr.write(`missing required arg ${USER_DATA_DIR_FLAG}<path>\n`);
    process.exit(2);
  }
  return found.slice(USER_DATA_DIR_FLAG.length);
}

// Synchronous, top-of-module: pin every per-app path before anything else
// (chrome-switches, name, async stdin read). If we wait until after
// `await readConfigLine()`, Electron's internal path resolver may have
// already readied and the calls become silent no-ops — that's how cookies
// were leaking to `~/Library/Application Support/Electron` and breaking
// auth persistence.
//
// What lives where, all under ~/.wavedash/dev-app-profile/:
//   ./                  cookies, localStorage, IndexedDB, Service Workers (userData + sessionData)
//   ./Cache/            Chromium HTTP disk cache (--disk-cache-dir)
//   ./Logs/             app.log et al
//   ./CrashDumps/       Crashpad output
const USER_DATA_DIR = findUserDataDirArg();
app.setName("Wavedash Dev");
// Append a token to the User-Agent so the wavedash site can detect that
// it's loaded inside Wavedash Dev — server-side via the `user-agent` request
// header, client-side via `navigator.userAgent`. Setting `userAgentFallback`
// here propagates to every webContents/session unless overridden, which we
// don't do anywhere. Mirrored by isWavedashDevApp() in
// wavedash/src/lib/wavedashDevApp.ts.
app.userAgentFallback = `${app.userAgentFallback} WavedashDev/${app.getVersion()}`;
app.setPath("userData", USER_DATA_DIR);
app.setPath("sessionData", USER_DATA_DIR);
app.setPath("logs", join(USER_DATA_DIR, "Logs"));
app.setPath("crashDumps", join(USER_DATA_DIR, "CrashDumps"));
app.commandLine.appendSwitch("user-data-dir", USER_DATA_DIR);
app.commandLine.appendSwitch("disk-cache-dir", join(USER_DATA_DIR, "Cache"));

function emit(message: object): void {
  process.stdout.write(JSON.stringify(message) + "\n");
}

function logErr(...args: unknown[]): void {
  process.stderr.write(args.map(String).join(" ") + "\n");
}

function readConfigLine(): Promise<RawConfig> {
  return new Promise((resolve, reject) => {
    let buffer = "";
    const onData = (chunk: Buffer): void => {
      buffer += chunk.toString("utf8");
      const idx = buffer.indexOf("\n");
      if (idx === -1) return;
      const line = buffer.slice(0, idx);
      process.stdin.off("data", onData);
      process.stdin.off("end", onEnd);
      try {
        const parsed = JSON.parse(line) as RawConfig;
        if (
          typeof parsed.uploadDir !== "string" ||
          typeof parsed.gameSubdomain !== "string" ||
          typeof parsed.playtestUrl !== "string"
        ) {
          reject(new Error("config missing required fields"));
          return;
        }
        resolve(parsed);
      } catch (err) {
        reject(err);
      }
    };
    const onEnd = (): void => {
      process.stdin.off("data", onData);
      reject(new Error("stdin closed before config received"));
    };
    process.stdin.on("data", onData);
    process.stdin.on("end", onEnd);
  });
}

function applyChromeSwitches(gameSubdomain: string, serverPort: number): void {
  app.commandLine.appendSwitch(
    "host-rules",
    `MAP ${gameSubdomain}:443 127.0.0.1:${serverPort}`,
  );
  // GPU: match chrome://flags/#{enable-unsafe-webgpu, enable-vulkan,
  // force-high-performance-gpu}. WebGPU games often need these for
  // dGPU dispatch + non-blocklisted adapter access.
  app.commandLine.appendSwitch("enable-unsafe-webgpu");
  app.commandLine.appendSwitch("enable-features", "Vulkan");
  app.commandLine.appendSwitch("force-high-performance-gpu");
}

/**
 * Trust our self-signed cert for the game subdomain only. Every other
 * hostname falls through to chromium's default verification — so HTTPS
 * to wavedash.com / third-party CDNs is verified normally.
 *
 * Safe because `--host-rules` guarantees that `<gameSubdomain>:443` resolves
 * to our 127.0.0.1 server inside this chromium instance. There is no path
 * by which a remote origin could impersonate it.
 */
function trustLocalCertFor(s: Session, gameSubdomain: string): void {
  s.setCertificateVerifyProc((request, callback) => {
    if (request.hostname === gameSubdomain) {
      callback(0); // 0 = accept
      return;
    }
    callback(-3); // -3 = use chromium's verification result
  });
}

function installAppMenu(window: BrowserWindow): void {
  // Reload/force-reload click handlers target window.webContents directly.
  // Default `role: 'reload'` targets the focused webContents, which is the
  // wrong one once DevTools is open.
  const isMac = process.platform === "darwin";
  const menu = Menu.buildFromTemplate([
    ...(isMac ? [{ role: "appMenu" as const }] : []),
    { role: "editMenu" as const },
    {
      label: "View",
      submenu: [
        {
          label: "Reload",
          accelerator: "CmdOrCtrl+R",
          click: () => window.webContents.reload(),
        },
        {
          label: "Force Reload",
          accelerator: "CmdOrCtrl+Shift+R",
          click: () => window.webContents.reloadIgnoringCache(),
        },
        {
          label: "Toggle Developer Tools",
          accelerator: isMac ? "Alt+Cmd+I" : "Ctrl+Shift+I",
          click: () => window.webContents.toggleDevTools(),
        },
        { type: "separator" as const },
        { role: "resetZoom" as const },
        { role: "zoomIn" as const },
        { role: "zoomOut" as const },
        { type: "separator" as const },
        { role: "togglefullscreen" as const },
      ],
    },
    { role: "windowMenu" as const },
  ]);
  Menu.setApplicationMenu(menu);
}

function attachContextMenu(window: BrowserWindow): void {
  window.webContents.on("context-menu", (_event, params) => {
    const hasSelection = params.selectionText.trim().length > 0;
    const isEditable = params.isEditable;
    const menu = Menu.buildFromTemplate([
      ...(isEditable
        ? [
            { role: "cut" as const, enabled: hasSelection },
            { role: "copy" as const, enabled: hasSelection },
            { role: "paste" as const },
            { type: "separator" as const },
          ]
        : hasSelection
          ? [{ role: "copy" as const }, { type: "separator" as const }]
          : []),
      { label: "Reload", click: () => window.webContents.reload() },
      { type: "separator" },
      {
        label: "Inspect Element",
        click: () => window.webContents.inspectElement(params.x, params.y),
      },
    ]);
    menu.popup({ window });
  });
}

async function bootstrap(): Promise<void> {
  const config = await readConfigLine();

  // Start the local server BEFORE app.whenReady so we can bake the chosen
  // port into `--host-rules`. Switches must be appended before chromium
  // launches, which it does as part of resolving the ready promise.
  let server: StartedServer;
  try {
    server = await startServer({
      uploadDir: config.uploadDir,
      gameSubdomain: config.gameSubdomain,
      verbose: !!config.verbose,
    });
  } catch (err) {
    logErr("failed to start local server:", err);
    app.exit(1);
    return;
  }
  if (config.verbose) {
    process.stderr.write(
      `local server listening on https://127.0.0.1:${server.port} (proxy for ${config.gameSubdomain})\n`,
    );
  }

  applyChromeSwitches(config.gameSubdomain, server.port);

  // When the CLI dies (or closes our stdin), shut ourselves down too.
  process.stdin.on("end", () => {
    app.quit();
  });

  await app.whenReady();

  trustLocalCertFor(session.defaultSession, config.gameSubdomain);

  // Packaged builds get their icon from build/icon.png via electron-builder,
  // which runs Apple's icon template at .icns generation time so the bundled
  // icon comes out as a squircle. In dev mode we set the dock icon ourselves,
  // so we have to feed it a pre-shaped PNG — feeding the raw square produces
  // a flat-edged dock icon. icon-rounded.png is that pre-shaped version.
  const icon = nativeImage.createFromPath(
    join(app.getAppPath(), "build", "icon-rounded.png"),
  );
  if (process.platform === "darwin" && app.dock && !icon.isEmpty()) {
    app.dock.setIcon(icon);
  }

  // Reserve a strip at the top of the page for the macOS traffic-light
  // buttons + a draggable region. `vibrancy: 'titlebar'` makes the window
  // background a translucent macOS material; the page covers most of it
  // (its own bg is opaque), but the strip below has `background-clip:
  // content-box` injected so the padding area stays transparent and the
  // vibrancy shows through. No `backgroundColor` here — vibrancy provides
  // it, and `show: false` + `ready-to-show` masks the brief pre-paint.
  const TITLEBAR_STRIP_PX = 38;
  const window = new BrowserWindow({
    width: 1280,
    height: 800,
    title: "Wavedash Dev",
    icon,
    // Vibrancy needs the window to actually be transparent to show through —
    // Electron's default backgroundColor is opaque white, which would mask it.
    backgroundColor: "#00000000",
    vibrancy: "titlebar",
    autoHideMenuBar: true,
    show: false,
    titleBarStyle: "hiddenInset",
    webPreferences: {
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });

  // Smooth load + traffic-light strip:
  //   - html opacity 0 → 1 fades the page in on every load (initial + reload).
  //   - body padding-top reserves space for the traffic lights so page
  //     content doesn't crowd them; background-clip: content-box keeps the
  //     padding transparent so vibrancy shows through.
  //   - body::before is the drag handle for that strip.
  // Inject on dom-ready (earliest reliable insertCSS hook). cssOrigin: 'user'
  // keeps page CSS from overriding it. Window stays hidden until ready-to-show.
  // !important on body rules: 'user' origin still loses to author rules in
  // the cascade unless flagged important; wavedash's Tailwind preflight
  // touches body and would otherwise win.
  const FADE_CSS = `
    html { opacity: 0; transition: opacity 220ms ease-out; }
    html.app-ready { opacity: 1; }
    body {
      padding-top: ${TITLEBAR_STRIP_PX}px !important;
      background-clip: content-box !important;
    }
    body::before {
      content: '';
      position: fixed;
      top: 0;
      left: 0;
      right: 0;
      height: ${TITLEBAR_STRIP_PX}px;
      -webkit-app-region: drag;
      z-index: 9999;
    }
  `;
  window.webContents.on("dom-ready", () => {
    void window.webContents.insertCSS(FADE_CSS, { cssOrigin: "user" });
  });

  window.once("ready-to-show", () => {
    window.maximize();
    window.show();
  });

  attachContextMenu(window);
  installAppMenu(window);

  window.webContents.openDevTools({ mode: "right" });

  // beforeunload listeners (mainsite GameRunnerComponent + SDK) silently
  // cancel reload in Electron unless we override them. Bind on the current
  // webContents AND future ones — web-contents-created alone misses the
  // already-constructed main window.
  const allowUnload = (event: Electron.Event): void => {
    event.preventDefault();
  };
  window.webContents.on("will-prevent-unload", allowUnload);
  app.on("web-contents-created", (_event, contents) => {
    contents.on("will-prevent-unload", allowUnload);
  });

  let readyEmitted = false;
  window.webContents.on("did-finish-load", () => {
    // Fade in on every load: the dom-ready insertCSS resets html opacity to 0
    // for each fresh document (initial load and reloads), so we have to flip
    // the app-ready class every time the load finishes — ready-to-show only
    // fires once for the window's lifetime, so it's not enough on its own.
    void window.webContents.executeJavaScript(
      `requestAnimationFrame(() => document.documentElement.classList.add('app-ready'))`,
    );
    if (!readyEmitted) {
      readyEmitted = true;
      emit({ type: "ready" });
    }
  });

  window.on("closed", () => {
    void server.close();
    emit({ type: "closed" });
    app.quit();
  });

  await window.loadURL(config.playtestUrl);
}

app.on("window-all-closed", () => {
  app.quit();
});

bootstrap().catch((err) => {
  logErr("bootstrap failed:", err);
  app.exit(1);
});
