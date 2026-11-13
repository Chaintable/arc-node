# Arc upstream v0.8.5 合并验证

日期：2026-11-13。生产及合并基线：`1ae1881d7561dadf75e0c18039c55329fcb25c53`（v0.8.4-ct.1）；upstream 目标：`v0.8.5@602eed3b702728cabb16be716a8fd5eb8e99a027`；测试代码与镜像：`6dae0c2630a7a4b3f75f5cd38051379a116ea497`。报告提交仅增加本文。

**结论：本轮合并验证通过。定向原生测试392/392、双架构CI通过；固定1000个不同canonical块的hash及完整四字段产物全部一致，0差异、0错误；测试节点已追平官方。等待用户review并以merge commit合入PR。**

## 1. Release 更新与升级理由

本轮范围为v0.8.4→v0.8.5。官方明确要求尽快升级，避免长期混跑；无硬分叉激活、无数据库迁移。

v0.8.5修复执行缓存对同块删除再重建账户的处理：账户经EIP-161删除、随后通过EIP-7702重建时，旧逻辑在原账户无代码的分支移除account cache后直接跳过，未写入重建后的account info及storage，后续读取可命中先前prewarm缓存的旧值。新逻辑仅在最终account info仍为空时跳过，重建账户继续走既有的post-block缓存更新。这里描述的是已核对的条件性缺陷，不表示已在生产观察到触发。

Reth由`circlefin/arc-reth v2.2.0-arc.1@1502324`更新至`v2.2.0-arc.2@508cbdf`，对应上游Reth #28148回补。六个增量commit中，运行时变更只有`crates/engine/execution-cache/src/cached_state.rs`；其余涉及CI与文档。Arc新增合法同块EIP-161删除/EIP-7702重建、缓存槽0→7的E2E回归，本轮已运行通过。

Malachite监控条目仍为v0.8.2，无新稳定版，实际编译pin也未变。目标v0.8.5的`prerelease=false`，与现行`/releases/latest`监控语义一致。官方compose通过环境变量接收镜像；其安装版本表未给出固定主网镜像pin，因此不把v0.8.5源码发布等同于官方主网已完成部署。

来源：[Arc v0.8.5 changelog](https://github.com/circlefin/arc-node/blob/v0.8.5/CHANGELOG.md)、[breaking changes](https://github.com/circlefin/arc-node/blob/v0.8.5/BREAKING_CHANGES.md)、[Reth #28148](https://github.com/paradigmxyz/reth/pull/28148)、[arc-reth增量](https://github.com/circlefin/arc-reth/compare/v2.2.0-arc.1...v2.2.0-arc.2)。

## 2. 保留的 fork patch 与双向影响

- 保留`pre_traceMany`、`eth_multiCall`、`trace_debankBlock`、Arc原生合约事件与child-frame completion采集，以及完整状态pass和gas/receipt/状态差异校验。14个运行时相关fork文件与生产基线逐字节一致；本次没有新增执行hook、缓存写入或并发入口。
- DeBank block trace分别从父块状态执行完整状态pass和trace pass。上游修正的是执行缓存更新，未改其输出协议；本轮完整比较header、block_file、validation_hash与六字段stateDiff，验证依赖升级对既有产出的兼容性。
- 保留运行镜像中的Bash/coreutils/procps及独立kill工具、同仓PR固定SHA镜像工作流。CI核对两架构镜像内完整commit及运行工具smoke；没有把这些smoke称为生产备份/prune流程的完整验收。
- Cargo.lock以目标v0.8.5为基底，只增加debank-rpc、md-5 0.10.6及3条必要fork依赖边。1308个上游包的版本、来源均保留；未全量更新依赖。
- 实时生产主/seed均为v0.8.4-ct.1，主节点二进制commit为1ae1881，与合并基线相同；没有遗漏独立生产维护线修复。

## 3. 测试机部署

- 独立测试目录：`/opt/apps/arc-upstream-v085/`，数据卷挂载于`writer/`。
- 来源为2026-11-13 00:01:52 UTC生产seed快照`snap-022e68fc1738cc34d`；本轮卷`vol-0b68286879c971ace`，200GiB gp3、6000IOPS、250MiB/s，初始化速率300MiB/s，初始化完成后启动节点。ext4检查及UUID更新完成，恢复后使用157G/197G，剩40G。
- EL于03:05:56 UTC启动，RPC/IPC就绪后03:06:36 UTC启动CL。EL/CL实际二进制完整SHA均为6dae0c2630a7a4b3f75f5cd38051379a116ea497，EL实际Reth为508cbdf。Storage V2快照初始高度30,941,272，新代码从30,941,273开始导入。
- 当前生产fork仅用于通用EL+CL，常规RPC由Leafage提供。生产投递在独立background-tracer v0.1.48；测试只运行EL+CL，不启ETL，不连接Kafka/etcd，不上传S3。未注入AWS凭据，节点禁用IMDS凭据回退；副本execution/discovery-secret已在首启前移出。
- 独立比较进程仅以ReadOnly session读取生产既有S3对象，未把凭据提供给节点。比较基准为chain5042/version88aa7766的四类产物，生成端对应生产v0.8.4-ct.1。另以两个近期生产RPC完整响应验证四类S3对象重建等价，均MATCH；不将这些预检计入正式1000块。
- 固定镜像为`arc-node:6dae0c26`与`arc-consensus:6dae0c26`；arc-node与arc-writer的multiarch manifest完全相同。EL/CL限制12/4GiB、4/2CPU；比较器2GiB/4CPU，采样器256MiB/0.25CPU。网络10.99.90.0/24查重通过，所有RPC/metrics端口仅绑定127.0.0.1。

<details>
<summary>测试 Compose 全文</summary>

```yaml
name: arc-upstream-v085
services:
  node:
    container_name: arc-upstream-v085-node
    image: public.ecr.aws/b2h7a5c4/chaintable/arc-node:6dae0c26
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
    - ./writer:/var/data
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
        max-size: 100m
        max-file: '5'
    networks:
    - arc
  consensus:
    container_name: arc-upstream-v085-consensus
    image: public.ecr.aws/b2h7a5c4/chaintable/arc-consensus:6dae0c26
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
    - ./writer:/var/data
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
        max-size: 100m
        max-file: '5'
    networks:
    - arc
  compare:
    container_name: arc-upstream-v085-compare
    image: arc-replay-buildenv:20261028
    profiles:
    - verify
    entrypoint:
    - /usr/bin/python3
    - -u
    - /work/runner.py
    working_dir: /work
    volumes:
    - ./evidence/comparison:/work
    - /usr/local/aws-cli:/usr/local/aws-cli:ro
    environment:
      AWS_DEFAULT_REGION: ap-northeast-1
      AWS_MAX_ATTEMPTS: '3'
      AWS_RETRY_MODE: standard
      AWS_SHARED_CREDENTIALS_FILE: /nonexistent
      AWS_CONFIG_FILE: /nonexistent
      AWS_EC2_METADATA_DISABLED: 'false'
    mem_limit: 2g
    cpus: 4
    restart: 'no'
    stop_grace_period: 1m
    logging:
      driver: json-file
      options:
        max-size: 100m
        max-file: '5'
    networks:
    - arc
  sampler:
    container_name: arc-upstream-v085-sampler
    image: arc-replay-buildenv:20261028
    profiles:
    - monitor
    entrypoint:
    - /usr/bin/python3
    - -u
    - /work/sampler.py
    working_dir: /work
    volumes:
    - ./evidence:/work
    environment:
      AWS_EC2_METADATA_DISABLED: 'true'
    mem_limit: 256m
    cpus: 0.25
    restart: unless-stopped
    stop_grace_period: 1m
    logging:
      driver: json-file
      options:
        max-size: 100m
        max-file: '5'
    networks:
    - arc
networks:
  arc:
    driver: bridge
    ipam:
      config:
      - subnet: 10.99.90.0/24
```

</details>

## 4. 编译与测试结果

| 验证 | 结果 |
|---|---|
| Rust 1.93.0格式及全workspace --locked检查 | PASS |
| 完整EL/CL binary build | PASS |
| DeBank RPC / Arc EVM单测 | 46 + 185 passed |
| RPC middleware / follow rpc_sync | 118 + 11 passed |
| 既有debank_regressions / DeBank E2E | 3 + 21 passed |
| EIP-161 / 新增账户重建缓存E2E | 1 + 1 passed |
| RPC gas cap / trace bytes | 5 + 1 passed |
| 定向原生测试合计 | **392 passed，0 failed**；filtered用例不计通过，未修改既有ignored配置 |
| Clippy，debank-rpc / arc-evm / arc-evm-node all-targets | PASS |
| 双架构CI [43354177213](https://github.com/Chaintable/arc-node/actions/runs/43354177213) | EL/CL×amd64/arm64、版本/运行工具smoke、两份multiarch manifest共6 job全部SUCCESS |
| 启动与持续同步 | EL/CL零重启、无OOM；未见panic/FATAL |
| 初始追块段hash预检 | 30,941,273–30,941,292，20/20 MATCH，不额外增加正式覆盖数 |
| 正式1000块hash与完整产物 | **1000/1000 MATCH，0 mismatch/error**；覆盖2511笔canonical交易 |
| 六字段stateDiff | 测试/参照各1000份，合计2000份全部成功解码并比较，无undecodable |
| 证据完整性复核 | 4000个生产原始对象的长度/SHA256检查通过，合计9,328,122字节 |
| 追平与近主网链头 | 03:26:41 UTC同高30,965,693；随后24次采样差距绝对值≤1块；2026-11-13T03:38:13.817601+00:00双端同高30,967,059，lag=0 |

03:31:20 UTC冻结1000个不同区块后才开始正式比较：起点连续800块30,941,273–30,942,072；在随后30,942,073–30,942,572的500块中按交易数选100块（3–10tx/block）；近head连续100块30,966,111–30,966,210。没有重复块凑数，没有因差异换样本。该release无新的分叉激活，账户删除/重建条件由新增原生E2E补充覆盖。

每块独立核对官方canonical hash、parent、stateRoot和交易ID/顺序；完整比较header、block_file、validation_hash和原始RLP stateDiff。仅忽略process_start_timestamp，storage_contracts及stateDiff集合字段按排序后的完整内容比较；stateDiff包含Hash、ParentHash、NewAccounts、DeletedAccounts、StorageDiff、NewCodes六字段，没有用header.stateRoot替代状态差异。

本地检查、构建、原生测试及Clippy进程耗时43m52s；正式对账进程119.8s（包括限速canonical header复核与产物采集比较），文件完整性审计另计。前轮v0.8.4的10000块结果未计入本轮。

冻结manifest SHA256：`b9fba38b28cfb7365fa60dddc5a5d021bb760c33e671d230d9b15f991e7a288e`。完整原始证据归档SHA256：`c38777dad913062e4c30d95efbbc8236f624ac0292809f259f5bde32634254d6`。执行器、双端原始数据、逐块结果、完整日志与资源状态保留在本轮本地任务归档，未上传测试资料到源码仓库。

## 5. 未完成项与后续

本轮约定的合并、原生构建/测试、双架构镜像、真实同步、1000块完整一致性及近head验证均已完成，没有未解决的验收失败。follow部署不覆盖validator投票及P2P共识运行。

[PR #16](https://github.com/Chaintable/arc-node/pull/16)等待用户review并以merge commit合入；之后另行按人审流程发布release及交付固定镜像。生产未修改，metadata未回写；测试资源与证据保留，清理仍走确认gate。
