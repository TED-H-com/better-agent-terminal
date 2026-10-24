import { test, expect } from '@playwright/test';

test.describe('Session mounts', () => {
  test('should verify the page title', async ({ page }) => {
    await page.goto('/');

    await expect(page).toHaveTitle(/Better Agent Terminal/);
  });
});
