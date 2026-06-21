"""Self-test: open the generated report.html in a real browser and verify it renders."""
import sys
from pathlib import Path
from playwright.sync_api import sync_playwright

report = Path("D:/workspase/rust/xlh/output/report.html").resolve()
url = report.as_uri()
shot = Path("D:/workspase/rust/xlh/output/report_screenshot.png")

errors = []
console_errors = []

with sync_playwright() as p:
    browser = p.chromium.launch(headless=True)
    page = browser.new_page(viewport={"width": 1200, "height": 1400})
    page.on("console", lambda m: console_errors.append(m.text) if m.type == "error" else None)
    page.on("pageerror", lambda e: console_errors.append(f"pageerror: {e}"))

    page.goto(url, wait_until="networkidle", timeout=60000)
    page.wait_for_timeout(1500)  # let ECharts finish rendering

    # 1. charts rendered as canvas
    charts = page.locator(".chart")
    n_charts = charts.count()
    canvases = page.locator(".chart canvas")
    n_canvas = canvases.count()
    if n_charts < 3:
        errors.append(f"expected >=3 .chart containers, found {n_charts}")
    if n_canvas < 3:
        errors.append(f"expected >=3 chart canvases (ECharts rendered), found {n_canvas}")

    # 2. metric cards present with non-empty values
    cards = page.locator(".card .value, .metric .value, .value")
    n_cards = cards.count()
    if n_cards < 5:
        errors.append(f"expected >=5 metric values, found {n_cards}")

    # 3. trades table has rows
    rows = page.locator("table tbody tr")
    n_rows = rows.count()
    if n_rows < 1:
        errors.append(f"expected >=1 trade row, found {n_rows}")

    # 4. canvas has real pixel size (actually drawn)
    sizes = canvases.evaluate_all("els => els.map(e => e.width * e.height)")
    if not sizes or any(s == 0 for s in sizes):
        errors.append(f"a chart canvas has zero area: {sizes}")

    # 5. title present
    title = page.title()

    page.screenshot(path=str(shot), full_page=True)
    browser.close()

print(f"URL              : {url}")
print(f"title            : {title}")
print(f".chart containers: {n_charts}")
print(f"chart canvases   : {n_canvas}  sizes(px area)={sizes}")
print(f"metric values    : {n_cards}")
print(f"trade rows       : {n_rows}")
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
print("\nPASS: report renders correctly")
