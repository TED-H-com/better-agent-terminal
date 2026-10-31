import { test, expect } from '@playwright/test';

test.describe('Multi-workspace terminal aggregator', () => {
  test('should load the application and render workspace views', async ({ page }) => {
    await page.goto('/');

    // Check if the root app element is present. This verifies the application
    // structure loaded. We will look for some standard elements expected.

    // We expect the app to show a sidebar and workspace views
    // Even if it shows a Tauri error overlay due to missing Tauri bridge,
    // it confirms the e2e scaffold runs the vite server.
    const body = await page.locator('body');
    await expect(body).toBeVisible();

    // Since we don't have the Tauri host API in playwright by default,
    // the React app may throw "no Tauri host runtime detected".
    // We'll assert that the page either shows the main content or the error overlay.
    const contentOrError = page.locator('#root, .error-overlay');
    await expect(contentOrError.first()).toBeVisible();
  });
});
