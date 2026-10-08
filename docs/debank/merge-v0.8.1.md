# Arc v0.8.1 upstream 合并验证

日期：2026-10-08。合并分支 `merge-v0.8.1`，审阅入口 [PR #14](https://github.com/Chaintable/arc-node/pull/14)。按用户指示，在测试节点追平链头前合并并发布 `v0.8.1-ct.1`；近链头 hash / trace 对比在合并时尚未完成，测试节点继续追块，结果另行补记。

## 1. 升级内容与必要性

[上游 v0.8.1](https://github.com/circlefin/arc-node/releases/tag/v0.8.1) 的 release notes 只有一条 #486，没有升级优先级或截止时间的说明，不含硬分叉，也不改已出块的执行结果。上游 main 在 v0.8.0 之后的其他 commit（#467 等）不在这个 tag 里。

#486 修的是 PQ（SLH-DSA-SHA2-128s）precompile `0x1800…0004` 的 gas 计费顺序。v0.8.0 先把三个 `bytes` 参数完整解码并复制出来，再扣 base gas（230,000）和 message gas；calldata 可以让三个 offset 指向同一个大数组，于是 gas 不足的调用也会先复制 3 倍输入再 out of gas。v0.8.1 先 tokenize、type check，扣完 gas 再复制。

新旧实现对任意输入的判定、gas used 和返回值相同，依据是源码：在锁定的 alloy-sol-types 1.5.7 中，旧版 `abi_decode_raw_validate` 就是 `abi::decode_sequence` → `type_check` → `detokenize` 三步（`src/types/function.rs:75`、`src/types/ty.rs:307`），新版调用同样的前两步，只把 `detokenize` 移到扣费之后；`bytes` 的 `type_check` 恒为真，`detokenize` 只复制已校验的切片、不会失败，message 长度在复制前后相同。所以非规范编码（offset 别名或不对齐、非零或缺失 padding、尾部多余字节）的接受与拒绝完全由同一个 `decode_sequence` 决定，200 gas 早期失败与扣费后失败的分界不变。上游的差分 proptest（512 个样本）是补充证据。该 precompile 在主网无 hardfork 门控，区块执行和 `eth_call`、`trace_debankBlock` 等路径都会走到。

必要性中：不升级也能正常同步，升级可以去掉这个放大开销，改动小、没有冲突、输出不变。

malachite 上游 10-06 发布了 v0.8.1，但 arc-node v0.8.1 仍 pin `tag = "v0.8.0"`，本次不涉及。

## 2. Merge 冲突与影响面

从 Chaintable `main=17634bc`（v0.8.0-ct.4 + PR #13）合入上游 `v0.8.1=ab09dbf`，merge commit `9a4fe71`，无冲突。上游改了 `crates/pq-precompile/{Cargo.toml,src/lib.rs}` 和 `Cargo.lock`（`arc-pq-precompile` 新增 `proptest` dev-dependency，一行）；与 fork patch 只在 `Cargo.lock` 重叠，git 自动合并。

保留的 fork patch（本次未改动）：

- `debank-rpc` crate 及 node 注册：`trace_debankBlock`（通用节点 ETL 拉取）、`pre_traceMany`、`eth_multiCall` 和查询类 RPC。
- `crates/evm`：保留 Arc subcall precompile 的子调用 trace；与 PQ precompile 无交集。
- Chaintable 公共 ECR 的 `build.yml` / `release.yml`，镜像加 shell 与独立 `kill`（PR #11、#13），版本元数据构建脚本。

## 3. 部署情况

生产 `production/blockchain-arc` 只读实测（2026-10-08）：只有通用节点。主 writer `nodex-node-88aa7766`（`arc-writer` / `arc-consensus:v0.8.0-ct.3`）和 seed `nodex-node-seed`（`v0.8.0-ct.4`，ct.3→ct.4 只改 Dockerfile），pod 为 init（删 `discovery-secret`）+ node（EL）+ consensus（CL，follow 模式，`--follow.endpoint=https://rpc.mainnet.arc.io/`）+ jrpcx。投递在独立的 `etl-88aa7766`，通过 `trace_debankBlock` 拉取；rpc / state-rpc 是 leafage-evm-x，读 Kafka/S3，不直连 writer。没有常规节点。

测试机 lihe-dev：

- 数据卷：seed 周备份快照 `snap-067827e3b3b3534db`（10-02 00:02Z，数据 145.2 GiB，高度 23,791,039）建 `vol-0d800f1fdb5a910c7`（150 GiB gp3，6000 IOPS / 250 MiB/s，初始化速率 300 MiB/s），挂 `/opt/app/arc/writer_merge_v0.8.1/data`。
- 只运行 EL 和 CL，不运行 etl / leafage，不连接 Kafka/S3。EL `--disable-discovery`，CL follow 模式不监听 p2p；首次启动前删除 `execution/discovery-secret`，EL 启动时（02:44:09Z）重新生成，新 enode 前缀 `dd852db9c29f739d`。端口只绑 127.0.0.1，独立网络 `10.99.76.0/24`。
- 镜像：PR CI run 37717180605 产出的 `public.ecr.aws/b2h7a5c4/chaintable/arc-node:9a4fe718`、`arc-consensus:9a4fe718`，二进制版本输出 commit `9a4fe71828e9`。
- 参数与生产相同，差别：EL 的 `--arc-rpc-upstream-url` 从同 pod 的 `127.0.0.1:31000` 改为 compose service 名 `consensus:31000`；宿主端口映射；`restart: on-failure:5`、`stop_grace_period: 5m`。

```yaml
# arc v0.8.1 merge 验证（PR #14 镜像 9a4fe718）。生产 production/blockchain-arc nodex-node-* 的 node + consensus 两容器。
# 只起 EL + CL；node 不投递（生产投递在独立 etl pod），本 compose 不含 etl/leafage，不连 Kafka/S3。
# 数据卷 vol-0d800f1fdb5a910c7（snap-067827e3b3b3534db，seed 10-02）挂在 ./data，容器内路径与生产一致（/var/data）。
name: arc-merge-v081

services:
  node:
    container_name: arc-merge-v081-node
    image: public.ecr.aws/b2h7a5c4/chaintable/arc-node:9a4fe718
    user: "999:999"
    entrypoint: ["/usr/local/bin/arc-node-execution"]
    command:
      - node
      - --chain=arc-mainnet
      - --datadir=/var/data/execution
      - --disable-discovery
      - --ipcpath=/var/data/run/reth.ipc
      - --auth-ipc
      - --auth-ipc.path=/var/data/run/auth.ipc
      - --http
      - --http.addr=0.0.0.0
      - --http.port=8545
      - --http.api=eth,net,web3,txpool,trace,debug,reth
      - --metrics=0.0.0.0:9001
      - --enable-arc-rpc
      - --arc-rpc-upstream-url=http://consensus:31000   # 生产同 pod 用 127.0.0.1:31000
      - --log.file.directory=/var/data/execution/logs
    environment:
      RUST_LOG: info
    volumes:
      - ./data:/var/data
    ports:
      - "127.0.0.1:18745:8545"
      - "127.0.0.1:29301:9001"
    mem_limit: 12g
    cpus: 4
    restart: on-failure:5
    stop_grace_period: 5m
    logging:
      driver: json-file
      options: {max-size: "100m", max-file: "5"}
    networks: [arc-merge-v081-net]

  consensus:
    container_name: arc-merge-v081-consensus
    image: public.ecr.aws/b2h7a5c4/chaintable/arc-consensus:9a4fe718
    user: "999:999"
    entrypoint: ["/usr/local/bin/arc-node-consensus"]
    command:
      - start
      - --home=/var/data/consensus
      - --eth-socket=/var/data/run/reth.ipc
      - --execution-socket=/var/data/run/auth.ipc
      - --rpc.addr=0.0.0.0:31000
      - --follow
      - --follow.endpoint=https://rpc.mainnet.arc.io/
      - --metrics=0.0.0.0:29000
      - --execution-persistence-backpressure
      - --execution-persistence-backpressure-threshold=10
    environment:
      RUST_LOG: info
      ARC_SYNC_PARALLEL_REQUESTS: "1"
      ARC_SYNC_BATCH_SIZE: "5"
      ARC_SYNC_REQUEST_TIMEOUT: "5s"
    depends_on:
      node: {condition: service_started}
    volumes:
      - ./data:/var/data
    ports:
      - "127.0.0.1:31010:31000"
      - "127.0.0.1:29010:29000"
    mem_limit: 4g
    cpus: 2
    restart: on-failure:5
    stop_grace_period: 5m
    logging:
      driver: json-file
      options: {max-size: "100m", max-file: "5"}
    networks: [arc-merge-v081-net]

networks:
  arc-merge-v081-net:
    driver: bridge
    ipam:
      driver: default
      config:
        - subnet: 10.99.76.0/24
```

## 4. 部署后测试情况

- 构建：`cargo check --workspace --all-targets --locked` 通过；`cargo test -p arc-pq-precompile -p arc-precompiles --locked`：14/14（含上游差分测试 `reserve_first_matches_legacy_ordering`），68 passed 1 ignored。PR CI amd64 / arm64 两个镜像均成功。
- 启动：02:39:38Z 启动 EL，卷初始化期间 EL 打开数据库约 4.5 分钟，02:44:09Z 建出 IPC；此前 CL 等待 EL 超时按设计退出重启，EL 就绪后 02:44:29Z 启动 CL，之后 EL / CL restart 均为 0，日志无 ERROR。
- 追块：新代码从 23,791,040 开始执行。卷初始化完成后约 878 块/分钟（生产同步参数 `ARC_SYNC_PARALLEL_REQUESTS=1`、`ARC_SYNC_BATCH_SIZE=5`），主网约 2 块/s。07:56Z 本地 24,089,522，官方 24,868,539，落后 779,017 块；最近一小时净追约 16.5 块/s，预计 21:00Z 前后追平。
- 区块 hash（参照官方 RPC，chainId 5042）：23,791,040–059（新代码执行的头 20 块）20/20 一致，23,795,551–570 20/20 一致。近链头段在合并时尚未做。
- `trace_debankBlock`（参照生产主 writer `nodex-node-88aa7766-0`）：23,791,040、23,792,379–382 共 5 块，按流水线归一化规则（`scripts/pipe.py verify trace`：`storage_contracts` 排序、`state_diff` 以 stateRoot 代替）5/5 一致；同 5 块原始 JSON 只剔除 `process_start_timestamp` 后 5/5 逐字节一致。这 5 块是否含 PQ 调用未核实，PQ 的覆盖靠下面的定向调用。近链头段在合并时尚未做。
- PQ precompile 同块对比（块 23,793,335 至 24,086,814，测试节点 vs 生产主 writer，`eth_call` 与 `debug_traceCall` callTracer 完整响应）：
  - 直接调用 10 组：空输入、错误 selector、只有 selector、参数长度不合法、合法长度但签名无效（完整验签返回 false）、64 字节别名输入、1 MiB 别名输入配 4 档 gas，全部一致。其中 1 MiB / 500 万 gas 一组两边都止于 `intrinsic gas too low`，没有进入 precompile，实际进入 PQ 的是 9 组。
  - 用 state override 注入一段合约，以 0 / 200k / 300k / 1M gas `STATICCALL` precompile，body 为 64 字节和 1 MiB，共 8 组，全部一致；其中 0、200k 和 300k（1 MiB）三档在 precompile 内 out of gas，即 #486 改动的路径。直接对 precompile 发交易打不到这条路径：1 MiB calldata 的 EIP-7623 floor 约 1050 万 gas，留给 precompile 的 gas 总是够用。
  - 非规范 ABI 输入 13 类（尾部多余字节、非零 padding、offset 越界或为 u256 最大值、长度高位非零、长度超出 body、vk/sig 别名、全空、截断、只有 offset、会解码失败的不对齐 offset）× 直接调用 1M gas、STATICCALL 0 和 240k gas，全部一致。
  - 能成功解码的非规范布局 3 类（不对齐 offset、offset 指回 head、缺少 padding）× 直接调用、STATICCALL 0 / 240k / 1M gas，全部一致，并断言 callTracer 中确实有对 PQ 的调用。
  - 前面几组脚本只比较两边响应是否相同，不检查目标路径是否执行；以上结论已按 trace 中的 PQ 调用逐项核对。
- 交叉审查：Codex 与 Kimi 独立只读审查 PR head `9a4fe71`，均 approve，未发现本次引入的代码问题；两者都从 alloy-sol-types 1.5.7 源码确认 #486 前后等价。

## 5. 其他问题

- 上游 main 的 #467（CL 等待 EL 期间打开着 redb 且未装 SIGTERM handler，被 kill 后下次启动要全量 repair）不在 v0.8.1 里。部署时 EL 未建出 IPC 之前不要停 CL，生产 pod 重建时同样适用。
