from pathlib import Path
from playwright.sync_api import sync_playwright

BASE = "http://127.0.0.1:8765"
FIXTURES = [
    Path(r"E:\WXWORK\WXWork\1688857401657171\Cache\File\2026-03\03.txt"),
    Path(r"E:\WXWORK\WXWork\1688857401657171\Cache\File\2026-03\【双休_五险一金_底薪5K+高提成,急聘销售经理_青岛 8-13K】何琪辰 10年以上.pdf"),
]


def main() -> None:
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True)
        page = browser.new_page(viewport={"width": 1440, "height": 1000})
        responses = []
        errors = []
        page.on("response", lambda response: responses.append((response.status, response.url)))
        page.on("pageerror", lambda error: errors.append(str(error)))
        page.goto(BASE, wait_until="networkidle", timeout=30000)
        print("home", page.title())
        for fixture in FIXTURES:
            page.locator("#fileInput").set_input_files(str(fixture))
            page.locator("#analyzeBtn").click()
            page.wait_for_selector("#workbench.active", timeout=120000)
            page.wait_for_function(
                """() => document.querySelector('#maskedBadge')?.textContent === '预览已更新'""",
                timeout=120000,
            )
            page.wait_for_timeout(500)
            print(
                fixture.suffix,
                "original_children=", page.locator("#originalPreview > *").count(),
                "masked_children=", page.locator("#maskedPreview > *").count(),
                "original_images=", page.locator("#originalPreview img").count(),
                "masked_images=", page.locator("#maskedPreview img").count(),
                "original_tables=", page.locator("#originalPreview table").count(),
                "masked_tables=", page.locator("#maskedPreview table").count(),
                "badge=", page.locator("#maskedBadge").inner_text(),
            )
            page.screenshot(path=f"test-results/manual-real-{fixture.suffix[1:]}.png", full_page=True)
            page.locator("#wbBack").click()
            page.wait_for_selector("#home.active")
        print("errors", errors)
        print("bad", [item for item in responses if item[0] >= 400])
        browser.close()


if __name__ == "__main__":
    main()
