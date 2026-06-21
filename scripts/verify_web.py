#!/usr/bin/env python3
"""端到端：启动 xlh serve，浏览器填表单点运行，校验 iframe 内报告渲染。"""
import os
import sys

# IMPORTANT: Clear proxy env vars BEFORE importing playwright (and before any
# subprocesses inherit the environment).  An http_proxy pointing to a remote
# host will cause ERR_PROXY_CONNECTION_FAILED for 127.0.0.1 addresses.
# Playwright's Chromium backend picks up the env at launch time, so we must
# clean the environment first.
for _var in ("http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY"):
    os.environ.pop(_var, None)
os.environ["NO_PROXY"] = "127.0.0.1,localhost"
os.environ["no_proxy"] = "127.0.0.1,localhost"

import subprocess
import time
import socket
from pathlib import Path
from playwright.sync_api import sync_playwright

PORT = 18081
SHOT = Path("output/web_screenshot.png").resolve()


def wait_port(port, timeout=60):
    end = time.time() + timeout
    while time.time() < end:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=1):
                return True
        except OSError:
            time.sleep(0.5)
    return False


def main():
    srv = subprocess.Popen(
        ["cargo", "run", "--quiet", "--", "serve", "--port", str(PORT)],
        env=os.environ.copy(),
    )
    try:
        if not wait_port(PORT):
            print("FAIL: 服务未在超时内就绪")
            return 1
        errors = []
        with sync_playwright() as p:
            # Do NOT pass proxy={"server":"direct://"} — that paradoxically
            # triggers ERR_PROXY_CONNECTION_FAILED in some Chromium builds.
            # Clearing the HTTP_PROXY env var before launch (done above) is
            # the correct and sufficient fix.
            browser = p.chromium.launch(headless=True)
            page = browser.new_page()
            page.on("console", lambda m: errors.append(m.text) if m.type == "error" else None)
            page.goto(f"http://127.0.0.1:{PORT}/")
            page.wait_for_load_state("networkidle")

            # ---- 基金代码下拉搜索 ----
            # 等清单加载（FUNDS 非空）；最多等 10 秒，未就绪则跳过下拉断言（降级场景）
            funds_ready = False
            for _ in range(20):
                if page.evaluate("Array.isArray(window.FUNDS) && window.FUNDS.length > 0"):
                    funds_ready = True; break
                page.wait_for_timeout(500)
            if funds_ready:
                fc = page.locator('#f-single [name="fund_code"]')
                fc.fill("")
                fc.type("白酒", delay=30)
                page.wait_for_selector(".fund-dropdown.show .fund-item", timeout=10000)
                # 点选第一个含 161725 的项（白酒指数）
                page.click('.fund-dropdown.show .fund-item[data-code="161725"]')
                assert fc.input_value() == "161725", "点选后基金框应填入 161725"
                page.screenshot(path=str(Path("output/web_fundsearch.png").resolve()), full_page=True)
            else:
                print("WARN: 基金清单未就绪（无缓存且离线），跳过下拉断言")

            page.click("#run-single")  # 用默认表单值运行
            # 等 iframe 内出现报告
            frame = page.frame_locator("#result")
            frame.locator("canvas").first.wait_for(timeout=60000)
            # 断言 iframe 内含"总收益"
            body_text = frame.locator("body").inner_text(timeout=10000)
            assert "总收益" in body_text, "报告应含 总收益"
            box = frame.locator("canvas").first.bounding_box()
            assert box and box["width"] > 0 and box["height"] > 0, "图表 canvas 面积应 > 0"
            page.screenshot(path=str(SHOT), full_page=True)

            # ---- 单次 tab 选 RSI 策略 ----
            page.click('.tab[data-tab="single"]')
            page.select_option('#f-single select[name="strategy"]', "rsi")
            page.click("#run-single")
            rframe = page.frame_locator("#result")
            rframe.locator("canvas").first.wait_for(timeout=60000)
            rtext = rframe.locator("body").inner_text(timeout=10000)
            assert "总收益" in rtext, "RSI 报告应含 总收益"
            page.screenshot(path=str(Path("output/web_rsi.png").resolve()), full_page=True)

            # ---- 对比 tab ----
            page.click('.tab[data-tab="compare"]')
            page.click("#run-compare")  # 默认两行策略
            # Wait for compare button to re-enable (fetch completed)
            page.wait_for_selector("#run-compare:not([disabled])", timeout=60000)
            cframe = page.frame_locator("#result")
            cframe.locator("canvas").first.wait_for(timeout=30000)
            ctext = cframe.locator("body").inner_text(timeout=10000)
            assert "总收益" in ctext, "对比报告应含 总收益"
            page.screenshot(path=str(Path("output/web_compare.png").resolve()), full_page=True)

            # ---- 寻优 tab ----
            page.click('.tab[data-tab="optimize"]')
            page.click("#run-optimize")  # 默认网格 (smart_dca ma_window/k 多值)
            # Wait for the optimize button to become re-enabled (fetch completed)
            page.wait_for_selector("#run-optimize:not([disabled])", timeout=120000)
            ofr = page.frame_locator("#result")
            ofr.locator("body").filter(has_text="参数寻优").wait_for(timeout=10000)
            otext = ofr.locator("body").inner_text(timeout=10000)
            assert "参数寻优" in otext, "寻优报告应含 参数寻优标题"
            ofr.locator("canvas").first.wait_for(timeout=30000)
            page.screenshot(path=str(Path("output/web_optimize.png").resolve()), full_page=True)

            # ---- 数据同步 ----
            page.click("#sync-all")
            # 同步全部会逐只联网抓取已缓存基金，给足时间
            page.wait_for_selector("#sync-result div", timeout=120000)
            sync_text = page.locator("#sync-result").inner_text(timeout=10000)
            assert ("条新" in sync_text) or ("同步失败" in sync_text), "同步结果应出现条目"
            page.screenshot(path=str(Path("output/web_sync.png").resolve()), full_page=True)

            # ---- 诊断 tab ----
            page.click('.tab[data-tab="diagnose"]')
            page.fill("#diag-fund", "161725")
            page.click("#run-diagnose")
            page.wait_for_function("document.querySelector('#diag-result').innerText.indexOf('建议策略') >= 0", timeout=60000)
            diag_text = page.locator("#diag-result").inner_text(timeout=10000)
            assert "建议策略" in diag_text, "诊断应给出建议策略"
            body_text = page.locator("#panel-diagnose").inner_text(timeout=10000)
            assert "不构成" in body_text, "应有免责声明"
            page.screenshot(path=str(Path("output/web_diagnose.png").resolve()), full_page=True)

            browser.close()
        assert not errors, f"页面有 console error: {errors}"
        print("PASS: 三 tab + 基金搜索 + RSI 策略 + 数据同步 + 诊断 均正常")
        return 0
    finally:
        srv.terminate()
        try:
            srv.wait(timeout=10)
        except Exception:
            srv.kill()


if __name__ == "__main__":
    sys.exit(main())
