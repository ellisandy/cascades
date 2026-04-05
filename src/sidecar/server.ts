/**
 * Cascades render sidecar.
 *
 * Stateless HTTP server: POST /render { html, width, height, mode } → PNG bytes.
 * Puppeteer headless Chromium takes the screenshot; Sharp applies grayscale +
 * Floyd-Steinberg dithering (threshold 140, coefficients 7/16 5/16 3/16 1/16).
 *
 * Render modes:
 *   device      – grayscale → dither → negate  (1-bit e-ink output)
 *   einkPreview – grayscale → dither            (preview without negate)
 *   preview     – raw Puppeteer PNG             (no Sharp processing)
 *
 * The browser instance is kept alive between requests.
 */

import puppeteer, { type Browser } from "puppeteer";
import sharp from "sharp";

export type RenderMode = "device" | "einkPreview" | "preview";

export interface RenderRequest {
  html: string;
  width: number;
  height: number;
  mode: RenderMode;
}

const PORT = parseInt(process.env.SIDECAR_PORT ?? "3001", 10);
const VALID_MODES: RenderMode[] = ["device", "einkPreview", "preview"];

// ─── Browser lifecycle ────────────────────────────────────────────────────────

let browser: Browser | null = null;

async function getBrowser(): Promise<Browser> {
  if (browser?.connected) return browser;
  browser = await puppeteer.launch({
    headless: true,
    args: ["--no-sandbox", "--disable-setuid-sandbox", "--disable-dev-shm-usage"],
  });
  return browser;
}

export async function closeBrowser(): Promise<void> {
  if (browser) {
    await browser.close();
    browser = null;
  }
}

// ─── Puppeteer screenshot ─────────────────────────────────────────────────────

async function screenshotHtml(
  html: string,
  width: number,
  height: number
): Promise<Buffer> {
  const b = await getBrowser();
  const page = await b.newPage();
  try {
    await page.setViewport({ width, height, deviceScaleFactor: 1 });
    await page.setContent(html, { waitUntil: "networkidle0" });
    const buf = await page.screenshot({ type: "png" });
    return Buffer.from(buf);
  } finally {
    await page.close();
  }
}

// ─── Floyd-Steinberg dithering ────────────────────────────────────────────────
// Threshold 140; error diffusion coefficients: right 7/16, BL 3/16, B 5/16, BR 1/16.

async function ditherBuffer(raw: Buffer): Promise<Buffer> {
  const { data, info } = await sharp(raw)
    .grayscale()
    .raw()
    .toBuffer({ resolveWithObject: true });

  const w = info.width;
  const h = info.height;

  // Float32 for error accumulation; initialised from uint8 pixel values.
  const px = new Float32Array(data);

  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const i = y * w + x;
      const old = Math.max(0, Math.min(255, px[i]));
      const nw = old < 140 ? 0 : 255;
      const err = old - nw;
      px[i] = nw;

      if (x + 1 < w)           px[i + 1]     += (err * 7) / 16;
      if (y + 1 < h) {
        if (x > 0)             px[i + w - 1] += (err * 3) / 16;
                               px[i + w]     += (err * 5) / 16;
        if (x + 1 < w)         px[i + w + 1] += (err * 1) / 16;
      }
    }
  }

  const out = Buffer.alloc(w * h);
  for (let i = 0; i < w * h; i++) {
    out[i] = Math.max(0, Math.min(255, Math.round(px[i])));
  }

  return sharp(out, { raw: { width: w, height: h, channels: 1 } })
    .png()
    .toBuffer();
}

// ─── Public render function ───────────────────────────────────────────────────

export async function render(req: RenderRequest): Promise<Buffer> {
  const raw = await screenshotHtml(req.html, req.width, req.height);

  if (req.mode === "preview") return raw;

  const dithered = await ditherBuffer(raw);

  if (req.mode === "einkPreview") return dithered;

  // device: negate so black pixels fire e-ink dots
  return sharp(dithered).negate().png().toBuffer();
}

// ─── HTTP server ──────────────────────────────────────────────────────────────

function isValidRequest(body: unknown): body is RenderRequest {
  if (typeof body !== "object" || body === null) return false;
  const b = body as Record<string, unknown>;
  return (
    typeof b.html === "string" &&
    typeof b.width === "number" &&
    typeof b.height === "number" &&
    typeof b.mode === "string" &&
    VALID_MODES.includes(b.mode as RenderMode)
  );
}

if (import.meta.main) {
  const server = Bun.serve({
    port: PORT,
    async fetch(req) {
      const { method, url } = req;

      if (method !== "POST" || new URL(url).pathname !== "/render") {
        return new Response("Not Found", { status: 404 });
      }

      let body: unknown;
      try {
        body = await req.json();
      } catch {
        return new Response("Bad Request: invalid JSON", { status: 400 });
      }

      if (!isValidRequest(body)) {
        return new Response("Bad Request: missing or invalid fields", { status: 400 });
      }

      try {
        const png = await render(body);
        return new Response(png, {
          status: 200,
          headers: {
            "Content-Type": "image/png",
            "Cache-Control": "no-store",
          },
        });
      } catch (err) {
        console.error("Render error:", err);
        return new Response("Internal Server Error", { status: 500 });
      }
    },
  });

  console.log(`Render sidecar listening on port ${server.port}`);

  const shutdown = async () => {
    await closeBrowser();
    server.stop();
  };
  process.on("SIGTERM", shutdown);
  process.on("SIGINT", shutdown);
}
