"""Self-test: open the generated compare.html and verify it renders."""
import sys
from pathlib import Path
from playwright.sync_api import sync_playwright

report = Path("D:/workspase/rust/xlh/output/compare.html").resolve()
url = report.as_uri()
shot = Path("D:/workspase/rust/xlh/output/compare_screenshot.png")

EXPECTED_RUNS = 3
errors = []
console_errors = []

with sync_playwright() as p:
    browser = p.chromium.launch(headless=True)
    page = browser.new_page(viewport={"width": 1200, "height": 1500})
    page.on("console", lambda m: console_errors.append(m.text) if m.type == "error" else None)
    page.on("pageerror", lambda e: console_errors.append(f"pageerror: {e}"))

    page.goto(url, wait_until="networkidle", timeout=60000)
    page.wait_for_timeout(1500)

    n_charts = page.locator(".chart").count()
    canvases = page.locator(".chart canvas")
    n_canvas = canvases.count()
    if n_charts < 2:
        errors.append(f"expected >=2 .chart containers, found {n_charts}")
    if n_canvas < 2:
        errors.append(f"expected >=2 chart canvases, found {n_canvas}")

    rows = page.locator("table tbody tr")
    n_rows = rows.count()
    if n_rows != EXPECTED_RUNS:
        errors.append(f"expected {EXPECTED_RUNS} comparison rows, found {n_rows}")

    # legend entries should include run names
    body = page.content()
    for name in ["普通定投", "智能定投", "均线择时"]:
        if name not in body:
            errors.append(f"run name missing from page: {name}")

    # best-cell highlight present
    if page.locator(".best").count() < 1:
        errors.append("no best-column highlight cells found")

    sizes = canvases.evaluate_all("els => els.map(e => e.width * e.height)")
    if not sizes or any(s == 0 for s in sizes):
        errors.append(f"a chart canvas has zero area: {sizes}")

    title = page.title()
    page.screenshot(path=str(shot), full_page=True)
    browser.close()

print(f"URL              : {url}")
print(f"title            : {title}")
print(f".chart containers: {n_charts}")
print(f"chart canvases   : {n_canvas}  sizes(px area)={sizes}")
print(f"comparison rows  : {n_rows} (expected {EXPECTED_RUNS})")
print(f"best-highlight    : {page if False else ''}")
print(f"console errors   : {len(console_errors)}")
for e in console_errors[:10]:
    print("   !", e)
print(f"screenshot       : {shot} ({'exists' if shot.exists() else 'MISSING'})")

if errors or console_errors:
    print("\nFAIL:")
    for e in errors:
        print("  -", e)
    if console_errors:
        print("  - console/page errors present")
    sys.exit(1)
print("\nPASS: compare report renders correctly")
