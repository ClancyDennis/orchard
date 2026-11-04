export function purchaseLinks(product: string) {
  const match = product.match(
    /^iPhone Duo\s+(256GB|512GB|1TB|2TB)\s+(Star White|Night Sky)$/i,
  );
  if (!match) return null;
  const storage = match[1].toLowerCase();
  const color = match[2].toLowerCase().replaceAll(" ", "-");
  const path = `buy-iphone/iphone-duo/7.6-inch-display-${storage}-${color}-unlocked`;
  return {
    standard: `https://www.apple.com/shop/${path}`,
    education: `https://www.apple.com/us-edu/shop/${path}`,
  };
}
