# DeBank Custom RPC 测试计划（Arc）

> 验证 `pre_traceMany` / `eth_multiCall` / `trace_debankBlock` 三个 RPC 在 Arc 节点上的功能与数据正确性。
> 计划参照 Tempo `docs/test-plan-generic-node.md`，针对 Arc 链特性（无 AA tx、USDC 作 native、`0x1800...` 自定义 precompile、EWMA base fee）改写。
> 起始日期：2026-05-15。镜像验证待 PR #1 CI 出包后部署。

## 测试环境

| 字段 | 值 |
|---|---|
| Host | `chaindev-misc-g1` (ap-northeast-1a, im4gn.4xlarge arm64) |
| RPC | `http://127.0.0.1:8545`（g1 内网；本地 ssh tunnel 后直连） |
| CL RPC | `http://127.0.0.1:31000` |
| Metrics | EL `:9001` / CL `:29000` |
| Chain ID | `5042002` (`0x4cef52`) |
| Mode | Archive (from genesis) |
| 待验证镜像 | `294354037686.dkr.ecr.ap-northeast-1.amazonaws.com/blockchain/arc-x:<sha>` (CI 产物，来源 PR #1) |
| 官方对照 RPC | `https://rpc.testnet.arc.network` |

**本地 tunnel**：
```sh
ssh -fNL 8545:127.0.0.1:8545 -L 31000:127.0.0.1:31000 -L 9001:127.0.0.1:9001 chaindev-misc-g1
RPC=http://127.0.0.1:8545
OFF=https://rpc.testnet.arc.network
```

## 待选测试区块

部署新镜像后从当前链头往回挑选下列类型的区块作为固定测试样本，记录到 `~/code/task_arc/arc-test-blocks.txt`（持久保留以便回归用）：

| 类型 | 选块条件 | 用途 |
|---|---|---|
| genesis | block 0 | section 11.1 |
| 空块 | tx count == 0 | section 11.2 |
| 标准交易块 | 含 ≥3 笔 EIP-1559 tx | section 3-7 主测 |
| EIP-2930 tx 块 | canonical tx 含 access list | 3.2 类型覆盖；BlockFile 当前不承载 access list |
| Legacy tx 块 | tx type=0x0 | 3.2 类型覆盖 |
| EIP-7702 tx 块（可选） | tx type=0x4，链上若有 | 3.2 类型覆盖 |
| Revert tx 块 | receipt status=0x0，revert 前无 EVM log | 6.1, 6.6 |
| Revert tx 含 log 块 | receipt status=0x0，revert 前 emit log | 6.9 |
| CREATE tx 块 | receipt 含合约部署 | 4.2.4 |
| `0x1800...` 调用块 | 主动调用 5 个 Arc 自定义 precompile 之一 | 11.6 |
| USDC native ↔ ERC-20 块 | 含 USDC 转账（任一接口） | 11.7 |
| 大块（gas_used 接近 limit） | gas_used > 80% limit | 13 性能 |

# 测试结果概要

镜像/部署/选块完成前所有测试均为待执行。

| 大类 | 测试点 | 通过 | 失败 | 不适用 | 状态 |
|---|---|---|---|---|---|
| 1. 顶层结构 | 4 | – | – | – | pending |
| 2. block | 9 | – | – | – | pending |
| 3. txs | 22 | – | – | – | pending |
| 4. traces | 10 | – | – | – | pending |
| 5. events | 8 | – | – | – | pending |
| 6. error_traces/events | 9 | – | – | – | pending |
| 7. storage_contracts | 4 | – | – | – | pending |
| 8. state_diff (RLP) | 14 | – | – | – | pending |
| 9. header | 18 | – | – | – | pending |
| 10. validation / determinism | 7 | – | – | – | pending |
| 11. 特殊区块 | 11 | – | – | – | pending |
| 12. background-tracer 兼容 | 4 | – | – | – | pending |
| 13. 批量回归 | 5 | – | – | – | pending |
| 14. pre_traceMany | 8 | – | – | – | pending |
| 15. eth_multiCall | 11 | – | – | – | pending |
| **合计** | **144** | – | – | – | pending |

### Trace 类型覆盖

| 类型 | 状态 |
|---|---|
| call | pending |
| delegatecall | pending |
| staticcall | pending |
| create | pending |
| suicide | pending（Arc 禁止 SELFDESTRUCT 带 value 部署 — 历史块中是否完全没有 SELFDESTRUCT 待确认；如无样本则按 N/A 处理） |

### 已知的预期差异（Arc vs 标准 Ethereum / 标准 reth）

1. **block.prevrandao 恒为 0**：Arc 无 beacon chain，`Header.mix_hash = 0x000...`。trace 输出中无 prevrandao 字段，不影响 debankBlock 但需在 4.1 / 9 中确认 `mixHash=0x00..00`。
2. **EIP-4844 blob 禁用**：`blobGasUsed=0`、`excessBlobGas=0`。
3. **时间戳可能多块共享**：亚秒出块、秒级 header timestamp 可能相邻块相同。验证 `block_file.block.timestamp` 与 `eth_getBlockByNumber.timestamp` 一致即可，不假设严格递增。
4. **USDC 作 native**：`eth_getBalance` 返回 18-dec native USDC。`block_file.txs[*].value` 单位是 native wei（18-dec），消费方按 USDC 解读。
5. **`Header` 字段集**：Arc 用 alloy-consensus 1.7.3，无 `block_access_list_hash` / `slot_number`（Tempo 2.0.4 才有），section 9 不要列这两项。
6. **AA tx 不存在**：`DebankTransaction.calls` / `fee_token` / `fee_payer_signature` 在 JSON 中始终为 `null`（保留 schema 兼容 background-tracer Go 消费方，D3）。section 3 不测 AA 路径。
7. **Arc 直接写 journal 的 event**：Zero5+ native value transfer 和 SELFDESTRUCT 会绕过 Solidity `LOG*` opcode，NCA/custom precompile 还可能没有可见 call-trace node。`trace_debankBlock` 使用本地 event inspector 保存完整 `Log.address/topics/data`、frame 和 emission order；成功 event 必须逐项等于 `ExecutionResult.logs` 和 receipt logs。禁止再用 `exec_logs[N..]`、按内容匹配或统一挂到 root 的 fallback。EIP-7708 event emitter 固定为 `0xfffffffffffffffffffffffffffffffffffffffe`。
8. **Arc base fee 写在 parent extraData**：与 RPC 输出无关，不在 debankBlock 中暴露。
9. **5 个自定义 precompile `0x1800...0000-0004`**：调用它们的 trace 显示 to_addr = precompile 地址；per-trace `self_storage_change=false`（与 Tempo TIP-20 同因——precompile 不走 SSTORE opcode），block 级 `storage_contracts` 不受影响。section 7.3-7.4 验证。

---

## 1. DebankOutPut 顶层结构

| # | 测试项 | 验证方法 | 状态 |
|---|---|---|---|
| 1.1 | 返回结构完整性 | `jq 'has("block_file","header","state_diff","validation_hash")'` | pending |
| 1.2 | validation_hash 类型 | `jq '.validation_hash \| type == "number" and . != 0'` | pending |
| 1.3 | state_diff 格式 | 检查 `0x` 前缀 + 长度 > 10 | pending |
| 1.4 | header 一致性 | 7 个 root 字段（hash/parentHash/stateRoot/transactionsRoot/receiptsRoot/number/timestamp）与 `eth_getBlockByNumber` 对比 | pending |

---

## 2. block_file.block (DebankBlock)

| # | 字段 | 类型 | 对照来源 | 状态 |
|---|---|---|---|---|
| 2.1 | id | string(hex) | eth_getBlockByNumber.hash | pending |
| 2.2 | height | number | 请求的 block_id（十进制 = hex 转换） | pending |
| 2.3 | parent_id | string(hex) | eth_getBlockByNumber.parentHash | pending |
| 2.4 | base_fee_per_gas | number 或 null(genesis) | eth_getBlockByNumber.baseFeePerGas | pending |
| 2.5 | miner | string(address) | eth_getBlockByNumber.miner | pending |
| 2.6 | gas_limit | number | eth_getBlockByNumber.gasLimit | pending |
| 2.7 | gas_used | number | eth_getBlockByNumber.gasUsed | pending |
| 2.8 | timestamp | number | eth_getBlockByNumber.timestamp | pending |
| 2.9 | process_start_timestamp | number | 合理范围（近期 ms 时间戳，非 0） | pending |

---

## 3. block_file.txs (DebankTransaction)

### 3.1 字段类型验证（每字段在主测块上至少 1 笔 tx 验证）

| # | 字段 | 类型 | 对照 API / 字段 | 状态 |
|---|---|---|---|---|
| 3.1.1 | id | string | receipt.transactionHash | pending |
| 3.1.2 | from_addr | string(address) | receipt.from | pending |
| 3.1.3 | to_addr | string(address) | CALL 用 receipt.to；CREATE 用 receipt.contractAddress（成功和失败都不能写零地址） | pending |
| 3.1.4 | gas_limit | number | tx.gas | pending |
| 3.1.5 | gas_price | number | receipt.effectiveGasPrice | pending |
| 3.1.6 | gas_used | number | receipt.gasUsed | pending |
| 3.1.7 | status | boolean | receipt.status (0x1→true, 0x0→false) | pending |
| 3.1.8 | max_fee_per_gas | number | tx.maxFeePerGas（EIP-1559） | pending |
| 3.1.9 | max_priority_fee_per_gas | number | tx.maxPriorityFeePerGas（EIP-1559） | pending |
| 3.1.10 | input | string(hex) | tx.input | pending |
| 3.1.11 | nonce | number | tx.nonce | pending |
| 3.1.12 | idx | number | receipt.transactionIndex, 从 0 递增 | pending |
| 3.1.13 | value | string(hex U256) | tx.value | pending |
| 3.1.14 | access_list | null 或字段缺失 | 当前基础 BlockFile schema 不承载 EIP-2930/1559 access list；精确 warmup 另列 TODO | pending |

### 3.2 tx 类型覆盖

| # | 测试项 | 验证内容 | 状态 |
|---|---|---|---|
| 3.2.1 | Legacy tx (type=0x0) | gas_price>0, max_fee_per_gas=gas_price, max_priority_fee_per_gas=0 | pending |
| 3.2.2 | EIP-2930 tx (type=0x1) | gas/status/input 等基础字段正确；记录 access list 未承载的已知限制 | pending（链上若有） |
| 3.2.3 | EIP-1559 tx (type=0x2) | max_fee_per_gas>0, max_priority_fee_per_gas≥0 | pending |
| 3.2.4 | EIP-7702 tx (type=0x4) | 若链上存在，基础字段正确；记录 access list / authorizationList 未承载的已知限制 | pending（链上若有） |
| 3.2.5 | 成功 tx | status=true | pending |
| 3.2.6 | Revert tx | status=false | pending |
| 3.2.7 | txs 数量 | 与 eth_getBlockByNumber.transactions 数量一致 | pending |
| 3.2.8 | idx 顺序 | 从 0 严格递增，与区块内 tx 顺序一致 | pending |

### 3.3 D3 schema preservation 验证

| # | 测试项 | 验证内容 | 状态 |
|---|---|---|---|
| 3.3.1 | `calls` 字段始终为 null | 在 100 个连续块中 `jq '.. \| objects \| select(.calls != null)' \| length == 0` | pending |
| 3.3.2 | `fee_token` 字段始终为 null | 同上，`.fee_token != null` 计数为 0 | pending |
| 3.3.3 | `fee_payer_signature` 字段始终为 null | 同上 | pending |
| 3.3.4 | 字段存在 | 即使为 null，每笔 tx 的 JSON 中均包含 `calls`/`fee_token`/`fee_payer_signature` 这三个 key（schema 完整） | pending（依赖 `#[serde(skip_serializing_if=Option::is_none)]` 行为：如启用则 key 缺失，未启用则 key 为 null。需验证实际序列化结果与 background-tracer 期望一致） |

---

## 4. block_file.traces (DebankTrace)

### 4.1 字段验证（与 `trace_transaction` 逐字段对比，每字段在 ≥10 条 trace 上验证）

| # | 字段 | 类型 | trace_transaction 对应 | 状态 |
|---|---|---|---|---|
| 4.1.1 | id | string(MD5, 32 hex) | 无（DeBank 字段，需手算 MD5 验证） | pending |
| 4.1.2 | from_addr | string(address) | action.from | pending |
| 4.1.3 | gas_limit | number | action.gas | pending |
| 4.1.4 | input | string(hex) | action.input (call) / action.init (create) | pending |
| 4.1.5 | to_addr | string(address) | action.to (call) / result.address (create) | pending |
| 4.1.6 | value | string(hex U256) | action.value | pending |
| 4.1.7 | gas_used | number | result.gasUsed | pending |
| 4.1.8 | output | string(hex) | result.output (call) / result.code (create) | pending |
| 4.1.9 | type | string | type ("call"/"create") | pending |
| 4.1.10 | call_type | string | action.callType (call) / "" (create) | pending |
| 4.1.11 | tx_id | string(tx hash) | transactionHash | pending |
| 4.1.12 | parent_trace_id | string | 无（DeBank 字段，MD5 验证） | pending |
| 4.1.13 | pos_in_parent_trace | number | 无（DeBank 字段） | pending |
| 4.1.14 | self_storage_change | boolean | 无（SSTORE opcode 检测） | pending |
| 4.1.15 | storage_change | boolean | 无（含子 trace 传播） | pending |
| 4.1.16 | subtraces | number | subtraces | pending |
| 4.1.17 | trace_address | array[number] | traceAddress | pending |
| 4.1.18 | error | string | error (成功=null, 失败 "Reverted") | pending |

### 4.2 trace type 覆盖

| # | 测试项 | 验证内容 | 状态 |
|---|---|---|---|
| 4.2.1 | call 类型 | type="call", call_type="call" | pending |
| 4.2.2 | delegatecall 类型 | type="call", call_type="delegatecall" | pending |
| 4.2.3 | staticcall 类型 | type="call", call_type="staticcall" | pending |
| 4.2.4 | create 类型 | type="create", call_type="", to_addr=新地址 | pending |
| 4.2.5 | 深层嵌套 | trace_address 至少 ≥3 层 | pending |
| 4.2.6 | storage_change 传播 | 子 frame 成功执行 SSTORE → 父 storage_change=true；即使祖先随后 revert 也保留执行信号 | pending |
| 4.2.7 | CREATE2 类型 | 高层 type="create"、call_type=""；CREATE2 不使用非标准 type="create2" | pending |

### 4.3 ID 计算 & 唯一性

| # | 测试项 | 验证内容 | 状态 |
|---|---|---|---|
| 4.3.1 | trace id 算法 | id == MD5(tx_id + parent_trace_id + pos_in_parent_trace)（手算对照） | pending |
| 4.3.2 | root trace id | parent_trace_id="", pos=0, MD5 验证 | pending |
| 4.3.3 | id 区块内唯一 | unique 数 = 总数 | pending |

### 4.4 与 trace_transaction 一致性

| # | 测试项 | 验证方法 | 状态 |
|---|---|---|---|
| 4.4.1 | per-tx trace 总数 | `debankBlock 中该 tx 的 traces + error_traces` 数 == `trace_transaction(tx_hash)` 数 | pending |
| 4.4.2 | 11 字段逐条对比 | 至少 20 条 trace × 11 字段，全 MATCH | pending |

---

## 5. block_file.events (DebankEvent)

### 5.1 字段类型验证

| # | 字段 | 类型 | 对照 API / 字段 | 状态 |
|---|---|---|---|---|
| 5.1.1 | id | string(MD5, 32 hex) | 无（手算 MD5 验证） | pending |
| 5.1.2 | contract_id | string(address) | receipt.logs[].address | pending |
| 5.1.3 | selector | string(hex, topic[0]) | receipt.logs[].topics[0] | pending |
| 5.1.4 | topics | array[string] | receipt.logs[].topics[1:] | pending |
| 5.1.5 | data | string(hex) | receipt.logs[].data | pending |
| 5.1.6 | parent_trace_id | string | 必须指向实际产生 event 的可见 trace；EIP-7708 emitter 与执行地址不同，不能用 to_addr 推导 | pending |
| 5.1.7 | pos_in_parent_trace | number | 同 parent 下 positions 无重复且按序 | pending |
| 5.1.8 | idx | number | 区块内全局 log index，从 0 递增 | pending |

### 5.2 event 总数一致性（Arc 比 Tempo 更严格）

| # | 测试项 | 验证内容 | 状态 |
|---|---|---|---|
| 5.2.1 | success events 等于 receipt logs | 按 block/tx emission order 逐项比较 address/topics/data，数量与内容都相同 | pending |
| 5.2.2 | success event idx 唯一 | events 的 idx 无重复；error_events 不参与，因为协议规定其 idx=0 | pending |
| 5.2.3 | success event idx 连续 | events 的 idx 等于 `[0, 1, ..., N-1]`；所有 error_events.idx=0 | pending |
| 5.2.4 | 多 tx 跨 tx 连续 | tx0 idx=[0..a], tx1 idx=[a+1..b]，无间隔无重叠 | pending |

---

## 6. block_file.error_traces / error_events

| # | 测试项 | 验证方法 | 状态 |
|---|---|---|---|
| 6.1 | revert tx traces → error_traces | revert tx 的所有 traces 出现在 error_traces，不在 traces | pending |
| 6.2 | 全成功块 error_traces 为空 | 全部 tx status=0x1 → error_traces 长度 0 | pending |
| 6.3 | error_traces 字段完整 | 与 traces[0] 同 18 个字段 | pending |
| 6.4 | error_events 字段完整 | 与 events[0] 同 8 个字段 | pending |
| 6.5 | traces + error_traces = trace_transaction 总数 | per-tx 验证 | pending |
| 6.6 | success events = receipt logs | per-block 逐项验证；error_events 是 reverted frame 的执行记录，不属于 receipt | pending |
| 6.7 | error 字段非空 | error_traces 中 error 字段 == "Reverted" 或类似非空字符串 | pending |
| 6.8 | revert tx with EVM events 行为 | revert 前 emit 的 EVM events → 全部出现在 error_events（inspector 捕获）。receipt 中这些 log 被回滚不存在。Arc：`error_events == inspector_captured_events`（无 fee log 修正） | pending |
| 6.9 | 内部 revert（try/catch） | 成功 tx 中失败的子调用 traces 进 error_traces，其余进 traces；与 reth-x 行为一致 | pending |

---

## 7. block_file.storage_contracts

| # | 测试项 | 验证方法 | 状态 |
|---|---|---|---|
| 7.1 | 类型 | jq `type == "array"` | pending |
| 7.2 | 含 SSTORE 合约 | traces 中 storage_change=true 的 to_addr 出现在 storage_contracts | pending |
| 7.3 | `0x1800...` precompile 验证 | 触发 SYSTEM_ACCOUNTING / NCC 等的块：precompile 地址出现在 `storage_contracts`（block 级捕获正确），但对应 trace 的 self_storage_change=false（per-trace 信号对 Rust precompile 无效） | pending |
| 7.4 | 空块 | 仅含系统/纯转账 tx 的块：storage_contracts=[] 或仅含 native USDC 转账涉及的内部地址 | pending |

---

## 8. state_diff (RLP-encoded BlockStorageDiff)

### 8.1 结构

| # | 测试项 | 验证 | 状态 |
|---|---|---|---|
| 8.1.1 | RLP 可解码 | python rlp.decode 成功 | pending |
| 8.1.2 | hash | == debankBlock.header.stateRoot | pending |
| 8.1.3 | parent_hash | == eth_getBlockByNumber(parent).stateRoot | pending |

### 8.2 new_accounts

| # | 测试项 | 验证 | 状态 |
|---|---|---|---|
| 8.2.1 | address | H256 = keccak256(原始 EOA/合约地址) | pending |
| 8.2.2 | balance | U256 | pending |
| 8.2.3 | nonce | u64, ≥0 | pending |
| 8.2.4 | code_hash | H256，EOA = KECCAK_EMPTY，合约非空 | pending |
| 8.2.5 | 非空块有 new_accounts | 含 USDC 转账块：≥1 个新账户（USDC 转入未见过的地址） | pending |

### 8.3 storage_diffs

| # | 测试项 | 验证 | 状态 |
|---|---|---|---|
| 8.3.1 | address | H256 = keccak256(合约地址) | pending |
| 8.3.2 | diffs[].index | H256 = keccak256(slot) | pending |
| 8.3.3 | diffs[].value | U256 新值 | pending |
| 8.3.4 | 含 USDC 合约 | USDC 转账块：`storage_diffs` 出现 USDC `0x3600...0000` 的 balance/allowance slot 变化 | pending |
| 8.3.5 | 与 storage_contracts 对应 | `storage_diffs` 中的地址集合 ⊆ `storage_contracts`（hash 后） | pending |

### 8.4 new_codes

| # | 测试项 | 验证 | 状态 |
|---|---|---|---|
| 8.4.1 | code_hash | H256 = keccak256(code) | pending |
| 8.4.2 | code | Bytes = bytecode | pending |
| 8.4.3 | 仅含新部署 | 非部署块：new_codes 长度 == 0 | pending |
| 8.4.4 | 部署块 | CREATE 块：new_codes 长度 ≥1 | pending |

### 8.5 deleted_accounts

| # | 测试项 | 验证 | 状态 |
|---|---|---|---|
| 8.5.1 | 正常块 | deleted_accounts == 0 | pending |
| 8.5.2 | SELFDESTRUCT 块 | Arc 禁止部署期带 value selfdestruct，是否完全无 SELFDESTRUCT 待确认；若有 selfdestruct → deleted_accounts ≥1 | pending（unlikely sample） |

### 8.6 空块

| # | 测试项 | 验证 | 状态 |
|---|---|---|---|
| 8.6.1 | block 1 或其他空块 | new_accounts=0, storage_diffs=0, new_codes=0, deleted=0 | pending |

---

## 9. header (alloy Header)

> Arc 用 alloy-consensus 1.7.3。无 `block_access_list_hash` / `slot_number`（D17）。共 18 个字段需验证（vs Tempo 20）。

| # | 字段 | 验证 | 状态 |
|---|---|---|---|
| 9.1 | hash | == eth_getBlockByNumber.hash | pending |
| 9.2 | parentHash | 一致 | pending |
| 9.3 | stateRoot | 一致 | pending |
| 9.4 | transactionsRoot | 一致 | pending |
| 9.5 | receiptsRoot | 一致 | pending |
| 9.6 | number | 一致 | pending |
| 9.7 | gasLimit | 一致 | pending |
| 9.8 | gasUsed | 一致 | pending |
| 9.9 | timestamp | 一致 | pending |
| 9.10 | baseFeePerGas | 一致（Arc EWMA + extraData 写下一块 base fee，不影响本块字段） | pending |
| 9.11 | miner | 一致 | pending |
| 9.12 | logsBloom | 一致 | pending |
| 9.13 | nonce | 一致（恒为 `0x0000000000000000`） | pending |
| 9.14 | mixHash | 一致（Arc 恒为 `0x000...000`，因无 PREVRANDAO 信标随机） | pending |
| 9.15 | sha3Uncles | 一致（恒为 `0x1dcc...9347`） | pending |
| 9.16 | difficulty | 一致（恒为 `0x0`） | pending |
| 9.17 | extraData | 一致（Arc 写下一块 base fee 计算结果，每块均非空） | pending |
| 9.18 | parentBeaconBlockRoot | 一致（Arc = keccak256(RLP(parent_header))，非 SSZ beacon root） | pending |
| 9.19 | withdrawalsRoot | 一致（Arc 无 withdrawal，按 reth 默认值） | pending |
| 9.20 | blobGasUsed | 一致（Arc 禁用 4844 → 0x0） | pending |
| 9.21 | excessBlobGas | 一致（0x0） | pending |
| 9.22 | requestsHash | 一致（reth 通用，Prague EIP-7685） | pending |

注：`requestsHash` (alloy) vs `requestsRoot` (Go pipeline) JSON key 名不同，pipeline 消费方不使用，与 Tempo 同。

---

## 10. validation_hash 与确定性

| # | 测试项 | 验证 | 状态 |
|---|---|---|---|
| 10.1 | 类型 | jq `type == "number"` | pending |
| 10.2 | 非零 | ≠ 0 | pending |
| 10.3 | 算法 | 对每个 id 求 SHA1 并按整数求和，取十进制和的末 6 位 — 与代码对照 | pending |
| 10.4 | 幂等 | 同 block 调两次返回相同 validation_hash | pending |
| 10.5 | BlockFile 确定性 | 同 block 重放两次，将 `process_start_timestamp` 归一化后逐字段相同 | pending |
| 10.6 | StateDiff RLP 确定性 | 同 block 重放两次，`state_diff` 原始 bytes 完全相同 | pending |
| 10.7 | 集合排序 | `storage_contracts`、account/code/storage diff 外层及每个 storage slots 内层均按协议字段升序 | pending |

---

## 11. 特殊区块

| # | 测试项 | 验证方法 | 状态 |
|---|---|---|---|
| 11.1 | Genesis (block 0) | `trace_debankBlock("0x0")` → synthetic txs/traces 存在；state_diff 非空（含 USDC `0x3600...0000` 等 predeploy 的 code/balance/storage） | pending |
| 11.2 | 空区块 | 任一 tx 数为 0 的块：txs=[], traces=[], events=[], state_diff 仅含 base fee 相关 storage（若 SYSTEM_ACCOUNTING 触发） | pending |
| 11.3 | Base fee 计算块 | 检查相邻 5 块的 `baseFeePerGas` 变化平滑（EWMA） | pending |
| 11.4 | 多 tx 区块 | 检查 txs[].idx 严格递增 [0,1,2,...] | pending |
| 11.5 | CREATE 区块 | root trace type="create"；tx.to_addr 等于 receipt.contractAddress；state_diff.new_codes ≥1；成功/失败 CREATE 都覆盖 | pending |
| 11.6 | 自定义 precompile 调用块 | trace 5 个 `0x1800...` precompile 之一被调用的块：traces 含 to_addr in {NCA, NCC, SYSACCT, CALLFROM, PQ}；输出 result 非空 | pending |
| 11.7 | USDC native 转账块 | EOA→EOA native USDC 转账：traces 顶层 type="call" value > 0；events 含统一 Transfer log（contract_id = `0xfffffffffffffffffffffffffffffffffffffffe`） | pending |
| 11.8 | USDC ERC-20 转账块 | 调用 `0x3600...0000.transfer(...)`：traces 含 to_addr=USDC 合约；events 含 Transfer log；两种接口产生的 events 等价 | pending |
| 11.9 | 不存在的区块 | `trace_debankBlock("0xffffffff")` → JSON-RPC error，message 含 "not found" | pending |
| 11.10 | 最新区块 | `trace_debankBlock("latest")` → 当前链头，height 与 eth_blockNumber 一致 | pending |
| 11.11 | 共享时间戳块 | 找 timestamp 相同的连续 ≥2 块：debankBlock.timestamp 与 eth_getBlockByNumber.timestamp 一致；header.timestamp 也一致 | pending |

---

## 12. 与 background-tracer 兼容性

| # | 测试项 | 验证方法 | 状态 |
|---|---|---|---|
| 12.1 | JSON 可解析 | background-tracer dry-run 反序列化 DebankOutPut 不报错 | pending（需 binary） |
| 12.2 | dry-run | `background-tracer dry-run --rpc-address=... --start-block=X --end-block=X+5` | pending（需 binary） |
| 12.3 | 连续 parent_id 链 | 连续调用 10 块，校验每块 parent_id == 前一块 id | pending |
| 12.4 | 性能 | 单次调用 < 5s（目标对照 Tempo 12ms 基线） | pending |

---

## 13. 批量回归测试

针对 PR #1 CI 出包后的镜像，从合适起点（建议跳过 archive 初始化、距 tip 1000 块以远以避免重组）回放 200 块。

| # | 测试项 | 覆盖区块 | 状态 |
|---|---|---|---|
| 13.1 | tx 数量一致 | 200 blocks | pending |
| 13.2 | block hash 一致 | 200 blocks | pending |
| 13.3 | event idx 语义 | success events 按 block-global receipt order 从 0 连续递增；所有 error_events.idx=0 | pending |
| 13.4 | trace 数量一致（per-tx vs trace_transaction） | 200 blocks, 估 ~500 txs | pending |
| 13.5 | success events 的 address/topics/data 按顺序逐项等于所有 receipt logs；error_events 单独验证 frame/position | 200 blocks | pending |

---

## 14. pre_traceMany 测试

### 14.1 基本功能

| # | 测试项 | 验证方法 | 状态 |
|---|---|---|---|
| 14.1.1 | 单笔 tx 预执行成功 | post `pre_traceMany([{tx}])` → result[0].error=null, trace/logs/gas_used 非空 | pending |
| 14.1.2 | 多笔 tx 顺序状态可见 | 第一笔修改某 slot，第二笔读同 slot → 第二笔看到修改后值（state commit 行为） | pending |
| 14.1.3 | state_overrides 仅首笔生效 | 第二笔不应看到 override 后的 state（实测 take() 语义） | pending |
| 14.1.4 | block_overrides 透传 | 指定 timestamp，trace 中 BLOCK_TIMESTAMP opcode 读到 override 值 | pending |
| 14.1.5 | 错误码 | Halt → 1001/InsufficientBalance；Revert → 1002/Reverted；其他 → 1000/UnKnown | pending |

### 14.2 业务场景

| # | 测试项 | 验证方法 | 状态 |
|---|---|---|---|
| 14.2.1 | native USDC 转账 trace | EOA→EOA value=1 USDC → trace value 体现转账金额（18-dec wei） | pending |
| 14.2.2 | USDC ERC-20 转账 logs | 调 `0x3600...0000.transfer` → logs 含 Transfer event | pending |
| 14.2.3 | gas_used 估算 | 用 `gas_used × 4` 作为业务侧 gasLimit 估算 → 实际 tx 不应 out-of-gas | pending |

---

## 15. eth_multiCall 测试

### 15.1 基本功能

| # | 测试项 | 验证方法 | 状态 |
|---|---|---|---|
| 15.1.1 | 批量 call 返回顺序一致 | 3 个 call，results[i] 对应 requests[i] | pending |
| 15.1.2 | stats.blockNum/blockHash/blockTime | 与 eth_getBlockByNumber 一致 | pending |
| 15.1.3 | fast_fail=true | 第一个 call fail → 后续 results.code = -40015 (EVMFastFailed) | pending |
| 15.1.4 | fast_fail=false（默认） | 第一个 call fail → 后续仍执行 | pending |
| 15.1.5 | disable_cache | stats.cacheEnabled=false；不影响功能 | pending |
| 15.1.6 | 指定历史 block | block_number=老区块 → 该高度 state 上执行 | pending |

### 15.2 native sentinel `0xeeee...`

| # | 测试项 | 验证方法 | 状态 |
|---|---|---|---|
| 15.2.1 | balanceOf | call `0xeeee.balanceOf(EOA)` → result 与 `eth_getBalance(EOA)` 一致（U256 BE 32 bytes） | pending |
| 15.2.2 | totalSupply | call → result = U256(1) | pending |
| 15.2.3 | decimals | call → result = U256(18) | pending |
| 15.2.4 | name / symbol | call → ABI-encoded "ETH"（Tempo 默认，D2） | pending |
| 15.2.5 | 未知 selector | call `0xeeee` with unknown 4-byte → code = -40001 NativeMethodNotFound | pending |

---

## 16. 已知 Arc vs 标准 reth 行为差异（核对表）

| 项 | 标准 reth | Arc | 验证方法 |
|---|---|---|---|
| Gas token | ETH | USDC (18-dec native, 6-dec ERC-20) | eth_getBalance 与 USDC ERC-20 balanceOf 比例 = 1e12 |
| Finality | 概率性 | 确定性 <1s | trace 立即可查，无重组 |
| prevrandao | 信标随机 | 恒 0 | header.mix_hash |
| parentBeaconBlockRoot | SSZ beacon root | keccak256(RLP(parent)) | header 字段对比 |
| EIP-4844 blob | 启用 | 禁用 | blobGasUsed=0 |
| SELFDESTRUCT 部署带 value | 允许 | 禁止 | 部署测试合约 → revert |
| USDC blocklist | 无 | mempool×2 + runtime 三重 | 给黑名单地址发 tx → mempool 阶段被拒 |
| 块时间戳 | slot 派生 | wall-clock，可共享 | 拉 ≥10 连续块，至少 1 对相邻 timestamp 相同 |
| Fee market | EIP-1559 | EIP-1559 + EWMA | feeHistory 平滑度 |
| Header 字段集 | 含 block_access_list_hash | 不含（alloy 1.7.3） | header 字段枚举 |

---

## 17. 故障排查速查

| 现象 | 检查 |
|---|---|
| `trace_debankBlock` not found | EL `--http.api` 是否包含 `trace` ？我们部署的容器命令含 `trace,debug,reth` 等 |
| `pre_traceMany` not found | EL `--http.api` 是否包含 `pre` ？需手动加 |
| `eth_multiCall` not found | EL `--http.api` 含 `eth`（默认）即可 |
| 老块查 null | mdbx archive 同步状态：`eth_syncing` |
| 大块性能慢 | 单块 > 5s：检查 `parallel_requests` / inspector 配置 |
| success events 与 receipt.logs 不一致 | 检查 event inspector 的 journal/callback 去重、hidden precompile frame 和 emitter address；禁止按数量补尾 |
| validation_hash 相同但响应不同 | validation_hash 不覆盖 storage_contracts/state_diff；归一化 process_start_timestamp 后逐字段比较，并单独比较 RLP bytes |
| validation_hash 跨次调用不一致 | trace/event 顺序或 ID 不确定，必须排查 |

---

## 18. 部署执行步骤（PR #1 CI 出包后）

1. 等 `Build artifacts (DeBank)` workflow 出包，记录新 image tag（git short sha）
2. 在 g1 上更新 docker-compose EL service `image:` → 新 tag
3. `sudo docker compose -f /data/arc/docker-compose.yml up -d arc-execution`
4. 等同步追到 tip（`eth_syncing` → `false`）
5. 选定测试块（按 section "待选测试区块" 表填 `arc-test-blocks.txt`）
6. 按 section 1-15 顺序跑测试，记录结果到本表 `状态` 列
7. 失败项写入 `docs/todo.md` follow-ups，必要时开新 PR 修

---

## 附录 A：辅助变量（执行测试时复用）

```sh
RPC=http://127.0.0.1:8545
OFF=https://rpc.testnet.arc.network
CHAIN_ID=5042002

# Arc 原生稳定币
USDC=0x3600000000000000000000000000000000000000
EURC=0x89B50855Aa3bE2F677cD6303Cec089B5F319D72a

# 5 个自定义 precompile
NCA=0x1800000000000000000000000000000000000000
NCC=0x1800000000000000000000000000000000000001
SYSACCT=0x1800000000000000000000000000000000000002
CALLFROM=0x1800000000000000000000000000000000000003
PQ=0x1800000000000000000000000000000000000004

# native sentinel (debank-rpc multi_call)
NATIVE_SENTINEL=0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee

# 系统合约
PROTOCOL_CONFIG=0x3600000000000000000000000000000000000001
MULTICALL3=0xcA11bde05977b3631167028862bE2a173976CA11
PERMIT2=0x000000000022D473030F116dDEE9F6B43aC78BA3
```

## 附录 B：参考资料

- Tempo 测试计划：`~/code/task_tempo/docs/test-plan-generic-node.md`（132/136 PASS + 200-block batch 1557 项 0 失败）
- Tempo 实现：`~/code/task_tempo/crates/debank-rpc/`
- Arc 链分析：`~/code/task_arc/ARC_ANALYSIS.md`
- Arc 节点 Playbook：`~/code/task_arc/ARC_PLAYBOOK.md`
- Arc 决策日志：`docs/debank-rpc-notes.md`（D1-D19）
- PR：https://github.com/Chaintable/arc-node/pull/1
