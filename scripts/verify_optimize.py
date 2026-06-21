#!/usr/bin/env python3
"""用 Playwright 加载 optimize.html，校验排名表与 Top-N 叠图渲染。"""
import sys
from pathlib import Path
from playwright.sync_api import sync_playwright

HTML = Path("output/optimize.html").resolve()
SHOT = Path("output/optimize_screenshot.png").resolve()

def main():
    if not HTML.exists():
        print(f"FAIL: {HTML} 不存在，请先 cargo run -- --config optimize.toml")
        return 1
    errors = []
    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_page()
        page.on("console", lambda m: errors.append(m.text) if m.type == "error" else None)
        page.goto(HTML.as_uri())
        page.wait_for_load_state("networkidle")

        # 排名表至少有 1 行
        rows = page.locator("table tbody tr").count()
        assert rows >= 1, f"排名表应有数据行，实际 {rows}"

        # 收益率图含 canvas 且像素面积 > 0
        canvas = page.locator("#chart-return canvas").first
        box = canvas.bounding_box()
        assert box and box["width"] > 0 and box["height"] > 0, "图表 canvas 面积应 > 0"

        page.screenshot(path=str(SHOT), full_page=True)
        browser.close()

    assert not errors, f"页面有 console error: {errors}"
    print(f"PASS: 排名表 {rows} 行；叠图已渲染；截图 {SHOT}")
    return 0

if __name__ == "__main__":
    sys.exit(main())
