import { expect, test } from '@playwright/test'

test('loads the welcome screen without page errors', async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))

  await page.goto('/')

  await expect(page).toHaveTitle(/vTorrent 2\.0/)
  await expect(page.getByRole('heading', { name: 'vTorrent 2.0' })).toBeVisible()
  await expect(page.getByText('Create New Wallet')).toBeVisible()
  expect(errors).toEqual([])
})
