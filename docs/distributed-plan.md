# rdmatop 分布式多节点监控 — 实现方案 v1（已实现）

基于 v0.1.33（d4764c8）。本方案已全部实现于 feat/distributed 分支。

## 架构

节点侧: rdmatop --agent [--listen 127.0.0.1:7809] [--name NAME] [--token-file F] [--stdio]
  - 复用 sampler::collect()，快照序列化为 JSON lines 广播给所有连接
  - 广播线程每 pass 序列化一次，连接线程只做 writeln+flush

主节点: rdmatop --nodes SPEC（逗号分隔 host:port，可 name=host:port，@file 每行一条）
  - NodeManager: 本地节点 + 每远端一个 RemoteSampler（与本地 Sampler 同接口）
  - ALL 视图聚合各节点当前设备类；Enter 下钻 = 现有单节点 UI 原样

## 线协议 v1（JSON lines over TCP）

客户端 → agent: {"v":1,"token":"...","interval_ms":1000}（握手）
                    {"t":"interval","ms":500}（控制帧）
agent → 客户端: {"v":1,"ok":true,"host":...,"name":...,"pid":...,"caps":[...]}
                 {"t":"snap","mono_ns":...,"sys":{...},"stats":{...},...}

- mono_ns = agent 自连接建立起的单调钟纳秒；速率由相邻帧差值计算，
  节点间时钟不同步不影响正确性
- agent 是哑转发：只发原始计数器，无速率字段
- 帧上限 4MB；未知字段忽略；v!=1 拒绝

## 关键机制

1. MonoMap: mono_ns → Instant 仿射映射，apply_snapshot 速率计算零改动
2. SysInfo 上移到采样线程（src/sysinfo.rs），远端节点显示远端 /proc
3. NodeState 影子状态: apply_node_snapshot 用 legacy 字段做 scratch 折叠
   （folding 标志隔离 rolling_avg/recorder 副作用），active 节点镜像渲染
4. RemoteSampler: Connecting → Live → Disconnected，退避重连 1s→30s
5. token 常数时间比较；agent 默认只听 loopback

## 键位

ALL 视图: ↑↓ 导航, Enter 下钻, Tab 切设备类, n 节点选择器, q/Esc 退出
节点视图: 现有全部键位 + Esc 返回 ALL, 1..9/0 跳转节点

## 测试

- wire 往返/畸形 JSON/协议帧单测（CI，无需硬件）
- tests/distributed.rs: localhost 回环（真 agent 真 socket）握手+帧+断连
- 118 项测试全绿；cargo fmt --check + clippy -D warnings 干净
- 端到端冒烟: agent + --connect + --nodes TUI 渲染验证通过

## 已知限制（v1）

- agent interval 全局共享（多观看者）
- 无 TLS（token 起步，内网限定）
- 重连后速率归零一拍（baseline 重置，语义正确）
- SSH 传输（--stdio 已就绪，ssh:// 解析待 PR）
