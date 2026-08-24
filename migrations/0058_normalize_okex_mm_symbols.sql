UPDATE okex_mm_alpha.trades
SET symbol = LEFT(symbol, LENGTH(symbol) - 4)
WHERE market = 'swap'
  AND symbol LIKE '%SWAP';
