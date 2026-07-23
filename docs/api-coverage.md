# REST API 覆盖清单

本清单来自 9 个 notebook 的实际请求路径，并按当前交易所官方文档校正。项目只提供只读查询，不暴露下单、转账、借款、还款或账户设置接口。

## 账户映射

| 账户标识 | 来源 notebook | AccountMode / 客户端 |
| --- | --- | --- |
| binance_mm_ltp | making_market_analysis/binance_mm_single_exchange_ltp.ipynb | BinanceUsdmFutures / BinanceClient |
| binance_fr_ltp01 | funding_rate_analysis/binance_fr_analysis_ltp01.ipynb | BinancePortfolioMargin / BinanceClient |
| binance_fr_nova01 | funding_rate_analysis/binance_fr_analysis_nova01.ipynb | BinancePortfolioMargin / BinanceClient |
| binance_fr_nova02 | funding_rate_analysis/binance_fr_analysis_nova02.ipynb | BinancePortfolioMargin / BinanceClient |
| binance_fr_self | funding_rate_analysis/binance_fr_analysis.ipynb | BinancePortfolioMargin / BinanceClient |
| gate_fr_ltp | funding_rate_analysis/fr_ltpgate_ltp.ipynb | GateUnified / GateClient |
| bitget_fr_self | funding_rate_analysis/fr_ltpbitget.ipynb | BitgetUnified / BitgetClient |
| okx_fr_self | funding_rate_analysis/fr_okx_self.ipynb | OkxUnified / OkxClient |
| gate_fr_self | funding_rate_analysis/fr_ltpgate.ipynb | GateUnified / GateClient |

外部资金、自营、nova01、nova02 是账户身份，不是交易所 API 模式。每个身份必须使用独立凭证；共享同一交易所和出口 IP 池的账户应克隆同一个 Dispatcher，以共享 IP 级限频状态。

## Binance

| 能力 | REST API | 账户模式 |
| --- | --- | --- |
| 账户净值、余额、仓位 | GET /fapi/v2/account | USD-M Futures |
| 合约成交 | GET /fapi/v1/userTrades | USD-M Futures |
| 当前挂单 | GET /fapi/v1/openOrders | USD-M Futures |
| 本账户强平订单 | GET /fapi/v1/forceOrders；GET /papi/v1/um/forceOrders；GET /papi/v1/margin/forceOrders | USD-M Futures；Portfolio Margin |
| Portfolio Margin 账户快照 | GET /papi/v1/account | Portfolio Margin |
| 杠杆现货成交 | GET /papi/v1/margin/myTrades | Portfolio Margin |
| UM 合约成交 | GET /papi/v1/um/userTrades | Portfolio Margin |
| 资金费收入 | GET /papi/v1/um/income，incomeType=FUNDING_FEE | Portfolio Margin |
| UM 合约手续费率 | GET /fapi/v1/commissionRate；GET /papi/v1/um/commissionRate | USD-M Futures；Portfolio Margin |
| 借贷计息历史 | GET /papi/v1/margin/marginInterestHistory | Portfolio Margin |
| Spot MM 小时返佣入账 | GET /sapi/v1/asset/assetDividend | USD-M Futures 标准账户 |
| 标记价、当前资金费率 | GET /fapi/v1/premiumIndex | 公共 |
| 合约、现货估值价格 | GET /fapi/v2/ticker/price；GET /api/v3/ticker/price | 公共 |
| Portfolio Margin 抵押率 | GET /sapi/v1/portfolio/collateralRate | API key |

成交按交易所允许的时间窗口分块并继续使用 fromId 分页。资金费按 `page` 分页，使用同一
`incomeType` 内唯一的 `tranId` 去重并按时间排序。在线 income 文档标明只保留最近三个月；
实际可返回窗口可能更长，但不能据此假定从任意 `startTime` 开始的数据都完整。更早数据需走
UM futures transaction history 异步下载或本地历史 CSV。Spot MM 返佣从策略 `st_ms` 开始同步；
assetDividend 窗口若达到 500 条上限则自动二分，并按分发记录 `id` 幂等写入 `rebates` 表。

## Bybit UTA v5

| 能力 | REST API |
| --- | --- |
| 账户净值、资产、负债 | GET /v5/account/wallet-balance，accountType=UNIFIED |
| 现货及衍生成交 | GET /v5/execution/list，execType=Trade |
| 本账户强平订单 | GET /v5/order/history，筛选 createType=CreateByLiq/CreateByTakeOver_PassThrough |
| 资金费 | GET /v5/account/transaction-log，type=SETTLEMENT |
| 借贷利息 | GET /v5/account/transaction-log，type=INTEREST |
| 账户手续费率 | GET /v5/account/fee-rate |

正式运行时由 `BybitClient` 直接鉴权请求交易所。成交、资金费、借息和强平均按 7 天窗口切分，
使用 `nextPageCursor` 分页并按交易所唯一 ID 去重。强平只支持 linear/inverse 衍生品，并明确排除
`CreateByAdl_PassThrough`。现有 `scripts/import_bybit_*_history.py` 仅用于一次性历史回填，不是运行时数据源。

## Gate Unified

| 能力 | REST API |
| --- | --- |
| 账户净值、资产、负债 | GET /api/v4/unified/accounts |
| Unified 账户模式 | GET /api/v4/unified/unified_mode |
| 现货成交 | GET /api/v4/spot/my_trades，account=unified |
| USDT 合约账户、仓位 | GET /api/v4/futures/usdt/accounts；GET /api/v4/futures/usdt/positions |
| USDT 合约成交 | GET /api/v4/futures/usdt/my_trades_timerange |
| 本账户 USDT 合约强平历史 | GET /api/v4/futures/usdt/liquidates（私有鉴权） |
| 资金费、手续费、已实现盈亏 | GET /api/v4/futures/usdt/account_book，type=fund/fee/pnl |
| 账户现货、合约手续费率 | GET /api/v4/wallet/fee |
| 融资计息记录 | GET /api/v4/unified/interest_records，type=margin |
| 现货资金流水 | GET /api/v4/spot/account_book |
| 当前借款 | GET /api/v4/unified/loans |
| 预估借款利率 | GET /api/v4/unified/estimate_rate |
| 历史资金费率 | GET /api/v4/futures/usdt/funding_rate |
| 合约规格与估值价格 | GET /api/v4/futures/usdt/contracts；GET /api/v4/futures/usdt/tickers；GET /api/v4/spot/tickers |

Gate 合约私有请求自动带 X-Gate-Size-Decimal: 1，避免整数张数响应丢失小数精度。强平查询使用私有
`/liquidates`，不会误用返回市场全量数据的公共 `/liq_orders`。
历史接口按 Gate 的秒级参数查询后，再统一解析秒、毫秒及小数秒时间字段并按调用方的毫秒范围严格过滤，
避免首尾边界同一秒内的范围外记录进入同步结果。

## Bitget UTA v3

| 能力 | REST API |
| --- | --- |
| 账户信息、设置、资产 | GET /api/v3/account/info；GET /api/v3/account/settings；GET /api/v3/account/assets |
| 成交 | GET /api/v3/trade/fills |
| 本账户强平订单 | GET /api/v3/trade/history-orders，筛选 execType=liquidation |
| 通用财务流水 | GET /api/v3/account/financial-records |
| 资金费 | financial-records 的 CONTRACT_MAIN_SETTLE_FEE_USER_IN/OUT |
| 各交易对手续费率 | GET /api/v3/account/all-fee-rate |
| 杠杆利息 | financial-records 的 INTEREST_SETTLEMENT_OUT |
| 当前借贷数据 | GET /api/v3/trade/loan-data |
| 行情估值 | GET /api/v3/market/tickers |
| 杠杆借贷利率 | GET /api/v3/market/margin-loans |
| 抵押折扣率 | GET /api/v3/market/discount-rate |

成交、强平订单和财务流水按 30 天分块，并按 cursor 分页。强平订单来自账户私有历史订单并在本地
校验 `execType/delegateType`，不会使用市场级 `/api/v3/market/liquidations`。ProductCategory
区分 SPOT、MARGIN、USDT-FUTURES、COIN-FUTURES 和 USDC-FUTURES。

## OKX Unified

| 能力 | REST API |
| --- | --- |
| 账户净值、资产、负债 | GET /api/v5/account/balance |
| 账户模式 | GET /api/v5/account/config |
| 最近三个月成交 | GET /api/v5/trade/fills-history |
| 本账户全量、部分强平历史 | GET /api/v5/account/positions-history，type=3/4 |
| 当前和近三个月账单 | GET /api/v5/account/bills；GET /api/v5/account/bills-archive |
| 资金费 | 账单 type=8，当前与 archive 合并去重 |
| 现货、杠杆、永续及交割合约手续费率 | GET /api/v5/account/trade-fee |
| 已计借贷利息 | GET /api/v5/account/interest-accrued |
| 当前借贷利率 | GET /api/v5/account/interest-rate |
| 借贷额度、债务和可借余额 | GET /api/v5/account/interest-limits |
| 历史资金费率 | GET /api/v5/public/funding-rate-history |

旧 OKX notebook 长期未运行，不能作为当前参数规范。实现已改用官方的 billId/timestamp after 游标：
成交和账单使用 begin/end 过滤，interest-accrued、funding-rate-history 和 positions-history 从查询
结束时间向更早记录分页。强平查询分别请求 type=3（全量强平）和 type=4（部分强平），不混入 ADL。

## 手续费口径

- 历史实际手续费随成交明细返回：Binance 使用 commission / commissionAsset，Gate 使用 fee，Bitget 使用 feeDetail，OKX 使用 fee / feeCcy。持久化表 trade_fills 已提供 fee_amount、fee_asset 和 fee_usdt，无需重复建立手续费流水表。
- 新增的 fee-rate 方法查询账户当前 maker/taker 费率，用于预估未来交易成本；它不是历史实收手续费。费率会随 VIP 等级、交易对和优惠配置变化，使用时应按需刷新。
- 统一 TradingFeeRate 字段为 exchange、account_mode、market、instrument、maker_rate、taker_rate、fee_tier、fee_group、effective_at_ms 和 raw；正费率表示成本，负费率表示返佣。
- 每个策略 schema 的 trading_fee_rates 表保存统一费率快照。fee_rate_probe 默认输出统一格式，指定 --raw 可查看交易所原始响应，指定 --db-schema 可测试落表。

## 尚未覆盖

- Axum 服务目前只暴露健康检查、策略元数据和 PnL；交易所客户端尚未接入 HTTP 路由或定时同步任务。

## 边界与安全

- 所有交易所客户端只发 GET；Binance notebook 中用于探测抵押上限的 POST /sapi/v1/portfolio/asset-collection 未迁移。
- Dispatcher 只在明确收到 HTTP 429/418 时切换 IP 重试；传输错误不自动重试。
- 本项目没有复制 notebook 中的任何 key、secret 或 passphrase。凭证应从进程环境或密钥服务注入。
- 这些 notebook 中存在明文凭证痕迹，投入运行前应轮换相关 API key，并只授予 Read 权限及绑定允许的出口 IP。
- 历史数据保留期仍由交易所决定。例如 Binance Portfolio Margin UM income 文档标明只覆盖近三个月，OKX fills-history 和 bills-archive 也只覆盖近三个月；更早数据需要独立的归档下载流程。

## 官方文档

- [Binance REST API](https://developers.binance.com/en/docs/products/spot/rest-api)
- [Gate API v4](https://www.gate.com/docs/developers/apiv4/en/)
- [Bitget UTA API](https://www.bitget.com/api-doc/uta/intro)
- [OKX API v5](https://www.okx.com/docs-v5/)
