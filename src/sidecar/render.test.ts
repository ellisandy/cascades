/**
 * Acceptance tests for the render sidecar.
 *
 * These tests exercise the full Puppeteer + Sharp pipeline end-to-end.
 * A real Chromium process is launched by Puppeteer; expect ~5-10s on first run.
 *
 * Assertions:
 *   - PNG output dimensions match the requested width × height.
 *   - device mode output is ≤ 90 KB (e-ink size budget).
 *   - einkPreview and preview modes produce valid PNG bytes.
 */

import { afterAll, describe, expect, it } from "bun:test";
import sharp from "sharp";
import { closeBrowser, render } from "./server";

// ─── Fixture HTML ─────────────────────────────────────────────────────────────

const FIXTURE_HTML = `<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8" />
  <style>
    * { margin: 0; padding: 0; box-sizing: border-box; }
    body {
      width: 800px; height: 480px;
      background: white; color: black;
      font-family: sans-serif;
      display: flex; align-items: center; justify-content: center;
    }
    .card {
      width: 700px; padding: 40px;
      border: 4px solid black; border-radius: 12px;
    }
    h1 { font-size: 48px; margin-bottom: 16px; }
    p  { font-size: 24px; }
  </style>
</head>
<body>
  <div class="card">
    <h1>Cascades</h1>
    <p>Render sidecar acceptance fixture</p>
  </div>
</body>
</html>`;

// ─── Helpers ──────────────────────────────────────────────────────────────────

async function pngDimensions(buf: Buffer): Promise<{ width: number; height: number }> {
  const meta = await sharp(buf).metadata();
  if (!meta.width || !meta.height) {
    throw new Error(`sharp metadata missing dimensions: ${JSON.stringify(meta)}`);
  }
  return { width: meta.width, height: meta.height };
}

// ─── Tests ───────────────────────────────────────────────────────────────────

afterAll(async () => {
  await closeBrowser();
});

describe("render sidecar", () => {
  describe("device mode", () => {
    it("returns a PNG with the requested dimensions", async () => {
      const png = await render({ html: FIXTURE_HTML, width: 800, height: 480, mode: "device" });
      expect(png).toBeInstanceOf(Buffer);
      const { width, height } = await pngDimensions(png);
      expect(width).toBe(800);
      expect(height).toBe(480);
    });

    it("produces output ≤ 90 KB", async () => {
      const png = await render({ html: FIXTURE_HTML, width: 800, height: 480, mode: "device" });
      const kb = png.byteLength / 1024;
      expect(kb).toBeLessThanOrEqual(90);
    });
  });

  describe("einkPreview mode", () => {
    it("returns a PNG with the requested dimensions", async () => {
      const png = await render({ html: FIXTURE_HTML, width: 800, height: 480, mode: "einkPreview" });
      const { width, height } = await pngDimensions(png);
      expect(width).toBe(800);
      expect(height).toBe(480);
    });
  });

  describe("preview mode", () => {
    it("returns a PNG with the requested dimensions (no Sharp processing)", async () => {
      const png = await render({ html: FIXTURE_HTML, width: 800, height: 480, mode: "preview" });
      const { width, height } = await pngDimensions(png);
      expect(width).toBe(800);
      expect(height).toBe(480);
    });
  });

  describe("smaller viewport", () => {
    it("renders at half-horizontal dimensions (800×240)", async () => {
      const png = await render({ html: FIXTURE_HTML, width: 800, height: 240, mode: "device" });
      const { width, height } = await pngDimensions(png);
      expect(width).toBe(800);
      expect(height).toBe(240);
    });
  });

  // Issue #23 regression: with the old `networkidle0` strategy, parallel
  // renders raced on per-render font/image fetches and any one of them
  // could push past the (default) 30s setContent timeout, returning 500
  // to the upstream cascades server. Reproduced in the wild on a Pi Zero
  // 2 W with 5 parallel /image.png requests (2 of 5 timed out at 42s).
  //
  // The fix (domcontentloaded + bounded fonts.ready wait) means parallel
  // renders no longer block on whole-page network quiescence. This test
  // exercises the same parallel pattern. Loose timeout (45s) keeps it
  // green on a slow CI runner; pre-fix, all 4 would fail under load.
  describe("parallel renders (issue #23 regression)", () => {
    it(
      "completes 4 concurrent renders without timing out",
      async () => {
        const results = await Promise.all(
          Array.from({ length: 4 }, () =>
            render({ html: FIXTURE_HTML, width: 800, height: 480, mode: "device" }),
          ),
        );
        for (const png of results) {
          expect(png).toBeInstanceOf(Buffer);
          expect(png.byteLength).toBeGreaterThan(0);
        }
      },
      45_000,
    );

    it(
      "tolerates a never-resolving subresource (e.g. dead font CDN) without hanging the render",
      async () => {
        // A reference to a host that should never connect within the
        // bounded `domcontentloaded` window. Pre-fix, `networkidle0`
        // would wait the full 30s for this to drain; post-fix, we paint
        // and screenshot whatever's available without blocking.
        const html = `<!DOCTYPE html><html><head><style>
          @font-face { font-family: "Hangs"; src: url("http://10.255.255.1/never.woff2"); }
          body { font-family: "Hangs", sans-serif; }
        </style></head><body><h1>OK</h1></body></html>`;
        const png = await render({ html, width: 400, height: 240, mode: "preview" });
        expect(png).toBeInstanceOf(Buffer);
        expect(png.byteLength).toBeGreaterThan(0);
      },
      // Tight timeout: if this exceeds 20s, the fix isn't actually
      // bounding the wait and we've regressed to issue #23 behaviour.
      20_000,
    );
  });
});
