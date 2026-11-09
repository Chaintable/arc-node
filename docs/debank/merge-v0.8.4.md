# Arc upstream v0.8.4 合并验证

日期：2026-11-08。代码基线：`830ad37`（已包含 upstream v0.8.1）；测试代码：`e458275428f8f955788530ba0a7171158de244c3`。报告提交仅增加本文，测试镜像仍固定为代码 SHA `e4582754`。

**结论：已同步历史区间的执行与投递产出一致性通过；追平及近主网链头覆盖尚未完成，等待用户决定继续等待或接受缺口。没有标记完整验收通过。**

## 1. Release 更新与升级理由

官方 v0.8.4 明确建议尽快升级、避免长期混跑：本版修复增量 state-root 计算漏掉新 hashed entry 的问题，无硬分叉激活、无数据库迁移。这提高了升级优先级，区别于 v0.8.2/3 可以自行排期的表述。

- **v0.8.4：** Reth 从 `paradigmxyz/reth v2.2.0@88505c7` 改为 `circlefin/arc-reth v2.2.0-arc.1@1502324`。已核对两仓基底 tree 完全一致；增量只有 trie iterator seek 边界修复、相应回归用例及旧版测试接口适配。原逻辑把 walker key 具有 seek prefix 当作可以跳过，新逻辑要求恰好等于该 prefix，避免漏掉位于更深已存分支之前的新条目。原生回归验证 incremental root 与 full rebuild root 相同。
- **v0.8.3：** 更新 rustls-webpki、webpki roots、zstd 等依赖处理安全公告；未改变节点业务源码。crypto-bigint 的 audit ignore 是未解决依赖限制的记录，不算修复。
- **v0.8.2：** follow 请求取消时中止对应 fetch，静默 newHeads 订阅主动重连；改进 CL 同步、有效性判定及错误处理；修复 CallFrom 完成结果对 trace 的反馈、PQ 输入复制和数据库读取失败路径，并更新网络及安全依赖。
- **Malachite：** 主仓 lock 从 v0.8.0@72143f6 移到 v0.8.2@a453b32，实际包含中间 v0.8.1 的 WAL/投票重放、同步请求归属与释放、网络资源约束等修复，以及 v0.8.2 的 commit round、prevote timer 和 validator proof 兼容修复。metadata 已评估版本不等于此前主仓实际编译版本。当前验证部署为 follow，不覆盖 validator 投票及 P2P 共识运行。

兼容性变化随 v0.8.2 带入：trace 默认每交易 64MiB，数据库读事务默认超时 60s；eth_callMany 默认每请求 10 次调用、每调用 100 个 override 账户；模拟不能绕过 RPC gas cap；CL 范围响应改为分页对象。已有显式 sync batch=5、timeout=5s 的生产设置保持不变。本轮通过了相关原生用例和选定历史样本，不据此保证任意未来重型 trace 都不会触及预算。

本次评估范围固定为 v0.8.1→v0.8.4。另列生产差距：生产主/seed 仍为 v0.8.0-ct.3/ct.4，因此后续实际切换也会带入此前已发布的 v0.8.1 PQ gas/输入复制修复。

上游完整说明：[v0.8.4 changelog](https://github.com/circlefin/arc-node/blob/v0.8.4/CHANGELOG.md)、[breaking changes](https://github.com/circlefin/arc-node/blob/v0.8.4/BREAKING_CHANGES.md)、[Reth #28264](https://github.com/paradigmxyz/reth/pull/28264)、[Malachite release notes](https://github.com/circlefin/malachite/blob/v0.8.2/RELEASE_NOTES.md)。

## 2. 保留的 fork patch 与双向影响

- 保留 `pre_traceMany`、`eth_multiCall`、`trace_debankBlock` 及 Arc 原生合约/EIP-7708 事件捕获。生产 ETL 使用 trace_debankBlock，本轮比较其完整输出。
- 自定义 trace 需要原始 child output/gas、child 状态、最终 completion 状态及回滚后的 journal log 数量，才能保持 errtrace/error_events 归属和透明子调用语义。新增 `BeforeChildFrameEnd` 区分 spawned child 的镜像结果与即时包装结果，在 frame_end 前通知观察器，随后继续使用上游 completion 和 frame_end 顺序。观察器不改变执行结果；未安装 hook 时不捕获额外元数据。
- 保留生产 K8s wrapper 所需的 Bash/coreutils/procps runtime 和独立 kill，以及同仓 PR 的 SHA 镜像工作流。CI 验证两架构的工具可用性与镜像内完整 commit；这不是 prune/备份维护入口的完整验收。
- Cargo.lock 以 v0.8.4 为基底补齐 fork 依赖。package 集合只新增 debank-rpc 与 md-5 0.10.6，未删除或升级上游已有 package。
- 生产 ct.3、ct.4 对应提交均被本次基线 origin/main 包含；没有遗漏独立维护线修复。

## 3. 测试机部署

- 主机：lihe-dev。运行目录：`/opt/app/arc/writer_merge_v0.8.4/`。
- 快照：`snap-038bed66c40200d8c`，2026-11-06 00:02 UTC，来自生产 seed。恢复为本轮 tagged 测试卷 `vol-0a267d3f8888d3190`，200GiB gp3、6000 IOPS、250MiB/s、初始化速率 300MiB/s；初始化完成后用于验证。恢复文件系统使用约147G/197G。
- 快照已有 Storage V2；没有执行存储格式迁移。EL 初始高度 29,749,409，新代码从 29,749,410 开始导入；chainId=5042。先确认 EL RPC/IPC，再启动 CL。
- 只启动 EL+CL。生产投递在独立 ETL pod，本次不启动 ETL；不连接 Kafka/etcd，不上传 S3。未注入 AWS 生产凭证并禁用 IMDS 凭证回退；首启前删除副本 execution/discovery-secret。
- 镜像：`public.ecr.aws/b2h7a5c4/chaintable/arc-node:e4582754` 与 `public.ecr.aws/b2h7a5c4/chaintable/arc-consensus:e4582754`。
- 参照：官方 RPC（chainId 已核对）及 `production/blockchain-arc/nodex-node-88aa7766-0:8545`。生产仅只读，未执行 rollout。
- 内存限制 EL 12GiB、CL 4GiB；CPU 4+2。根盘余量小，本轮日志轮转为每容器20MiB×5，原始证据另存本轮任务目录。端口仅绑定127.0.0.1，独立网络10.99.90.0/24。

<details>
<summary>测试 Compose 全文</summary>

```yaml
# Arc v0.8.4 backup：EL+CL，代码镜像e4582754；根盘预算使日志轮转为20m×5，证据另存任务目录。
name: arc-merge-v084
services:
  node:
    container_name: arc-merge-v084-node
    image: public.ecr.aws/b2h7a5c4/chaintable/arc-node:e4582754
    user: 999:999
    entrypoint:
    - /usr/local/bin/arc-node-execution
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
    - --arc-rpc-upstream-url=http://consensus:31000
    - --log.file.directory=/var/data/execution/logs
    environment:
      RUST_LOG: info
      AWS_EC2_METADATA_DISABLED: 'true'
    volumes:
    - ./data:/var/data
    ports:
    - 127.0.0.1:18745:8545
    - 127.0.0.1:29301:9001
    mem_limit: 12g
    cpus: 4
    restart: on-failure:5
    stop_grace_period: 5m
    logging:
      driver: json-file
      options:
        max-size: 20m
        max-file: '5'
    networks:
    - arc-merge-v084-net
  consensus:
    container_name: arc-merge-v084-consensus
    image: public.ecr.aws/b2h7a5c4/chaintable/arc-consensus:e4582754
    user: 999:999
    entrypoint:
    - /usr/local/bin/arc-node-consensus
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
      ARC_SYNC_PARALLEL_REQUESTS: '1'
      ARC_SYNC_BATCH_SIZE: '5'
      ARC_SYNC_REQUEST_TIMEOUT: 5s
      AWS_EC2_METADATA_DISABLED: 'true'
    depends_on:
      node:
        condition: service_started
    volumes:
    - ./data:/var/data
    ports:
    - 127.0.0.1:31010:31000
    - 127.0.0.1:29010:29000
    mem_limit: 4g
    cpus: 2
    restart: on-failure:5
    stop_grace_period: 5m
    logging:
      driver: json-file
      options:
        max-size: 20m
        max-file: '5'
    networks:
    - arc-merge-v084-net
networks:
  arc-merge-v084-net:
    driver: bridge
    ipam:
      driver: default
      config:
      - subnet: 10.99.90.0/24
```

</details>

## 4. 编译与测试结果

| 验证 | 结果 |
|---|---|
| Rust 1.93.0，cargo fmt / diff check | PASS |
| cargo check --workspace --all-targets --locked | PASS |
| Arc EVM / PQ / precompiles / DeBank RPC 原生测试 | 185 + 16 + 67 + 46 = 314 passed，0 failed；原有1个unit ignored未改 |
| trie seek-boundary 原生回归 | 1 passed |
| DeBank e2e / 上游 CallFrom completion e2e | 21 + 2 passed |
| RPC middleware / follow rpc_sync | 118 + 11 passed |
| 既有 debank_regressions / gas cap / trace bytes | 3 + 5 + 1 passed |
| 定向原生与e2e合计 | **476 passed，0 failed**；原有doc-test ignored未改，未将 filtered 用例算作通过 |
| PR代码镜像 CI [42481012649](https://github.com/Chaintable/arc-node/actions/runs/42481012649) | EL/CL × amd64/arm64、multiarch manifest、版本与运行工具 smoke 全部PASS |
| 启动与同步 | EL/CL均运行，零重启、无OOM；未见panic/FATAL/ERROR，canonical高度持续增加 |
| 第一段 hash | 29,749,410–29,749,429：20/20 与官方一致 |
| 第二段已同步区间 hash | 29,760,300–29,760,319：20/20 与官方一致；不是近主网链头段 |
| trace_debankBlock | **100/100 与生产同块完整输出一致** |
| state_diff | 两端各100份均成功解码为六字段，无undecodable；后四字段按集合比较，没有用header.stateRoot代替 |
| 追平及近主网链头 hash/trace | **未完成，等待用户决策** |

100块样本包含新代码起点的10块，以及较后已同步区间的90块，其中纳入500块窗口内交易数最多的30块，最高20 tx/block。只忽略 process_start_timestamp；比较 header、validation_hash、block_file、storage_contracts 及完整 state_diff。原始双方响应与选块依据保存在本轮 run 目录。空块前后置状态、CREATE/CREATE2、回滚日志及特殊合约行为由定向e2e补充。

同时刻双端采样（2026-11-08T13:23:41.474569+00:00）：测试 29,765,968，官方 30,184,821，落后 418,853 块。13:15:01–13:20:01 UTC 的 canonical 高度从29,757,874到29,762,832，平均16.53块/s。以主网约2块/s持续增长估算，净追赶约14.53块/s，按当前速率仍需约8.0小时；这是条件性估计，不是完成保证。

估算依据为实际canonical块高和源码中的follow逐块导入路径，没有仅以Received new payload日志的序列速度推算，也没有把eth_syncing=false当作已追平。

## 5. 未完成项与后续

- 本轮已同步区间的hash、完整trace与六字段state_diff通过；追平及近主网链头覆盖仍未通过，不将其写成完整验收PASS。
- 等用户选择继续追平，或明确接受这两项缺口后进入PR review。测试容器与分钟采样继续保留，未清理现场。
- PR由用户review并以merge commit合入。之后再按人审流程发布release、交付固定镜像与SRE切换说明；生产切换不由本流水线执行。metadata尚未回写。
