# HeightField：数据来源与验收范围

`SceneState::render_scene()` 可将捕获中的 `PxHeightFieldGeometry` 和 `PxHeightField` 转为共享三角网格。地形高度、洞和拓扑来自 PVD 流，不运行物理仿真，也不在客户端重新烘焙 PhysX 地形。

## 源码证据

首条验证使用官方 PhysX 3.4.2 固定提交 `5e42a5f112351a223c19c17bb331e6c55037b8eb`：

| SDK 源文件 | 所证明的规则 |
| --- | --- |
| `PhysX_3.4/Source/PhysX/src/PvdMetaDataPvdBinding.cpp` | 注册 `PxHeightFieldSample`，发送 descriptor 的行列数、Format、Thickness，以及 `saveCells` 得到的 Samples 数组 |
| `PhysX_3.4/Include/geometry/PxHeightFieldSample.h`、`PxHeightFieldFlag.h` | 每个样本由 signed i16 height 和两个材质字节组成；material0 高位为 tessellation；低 7 位的 127 表示洞 |
| `PhysX_3.4/Source/GeomUtils/src/hf/GuHeightField.h` | 两种 quad 切分的顶点索引、行列次序 |
| 同目录 `GuHeightFieldUtil.cpp` | 形状空间缩放和正 Thickness 的三角形绕序翻转 |
| `PhysX_3.4/Include/geometry/PxMeshQuery.h` | `getTriangle` 可由 SDK 独立返回指定地形三角形的顶点及索引 |

PVD 类型元数据把 sample Height 声明为 unsigned 16 位，但其源数据是 signed 16 位。核心 `decode_value` 按端序读取后恢复 i16，几何提取严格检查其数值范围。

## 行为与边界

- 顶点坐标为 `[row × RowScale, height × HeightScale, column × ColumnScale]`，之后由通用场景映射合成 Shape local pose 和 Actor global pose。
- 同时支持两个 tessellation 方向，逐三角形排除洞；正 Thickness 使用 SDK 一致的绕序。渲染的是采样表面，不人为添加厚度侧面或底面。
- 几何缓存键包含资源 ID、资源代际、几何种类和全部缩放位。相同资源与缩放的多个 Shape 共用 `Arc` 顶点/索引；不同缩放不会互相污染。失效代际引用不会绑定到复用 ID 的新资源。
- 行列数至少 2，格式限定 `eS16_TM`。Samples 数量必须精确等于 rows × columns，每个样本有三个有效字段。溢出、截断、未知格式、非有限数值、非正缩放和缩放后溢出均拒绝转换，并保留原始对象属性供检查。
- 单网格最多 100 万顶点和 300 万索引；分配前按完整矩形的最坏索引数检查，含大量洞的超大矩形也受此限制。输出索引数组收缩到实际非洞数量，参与现有 128 MiB 唯一网格总预算。同一共享网格只计一次。
- 当前材质按通用 Actor 状态着色。材质编号仍保存在原始 Samples 属性中，不宣称支持逐三角形材质外观或地形纹理。

## 可重复的真实 SDK 验证

准备固定 SDK 的方法见 `tools/physx-sample/README.md`。在项目根目录执行：

```powershell
./tools/physx-features/build.ps1 -Target pvd-heightfield -BuildRoot .deps/physx-heightfield-build
./.deps/physx-heightfield-build/Release/pvd-heightfield.exe captures/physx-3.4-heightfield.pvdstream captures/physx-3.4-heightfield.heightfield.json
cargo test --locked -p pvd-core --lib mesh::tests
cargo test --locked -p pvd-core --test heightfield -- --ignored
```

集成测试也接受 `PVD_HEIGHTFIELD_CAPTURE`，同名 `.heightfield.json` 为真值。生产器直接调用 `PxMeshQuery::getTriangle` 并写入 SDK 返回的索引和世界顶点；没有复制 Rust 的三角化算法来生成预期结果。

样例包含 4 × 5 的非对称 signed heights、两个 tessellation 方向、四个洞三角形、正负 Thickness、非单位三轴缩放、Actor 与 Shape 的非单位姿态、两个共享同缩放资源的 Actor、不同缩放实例、运行中移动，以及销毁重建 Actor。75 帧跨越 60 帧检查点。

实际结果：

- 75 帧逆序随机恢复；4 个 Actor × 每个 20 个非洞三角形 × 75 帧，共 **6000 个三角形的索引和全部世界顶点与 SDK 真值一致**。
- 顺序重建与检查点回放一致；共享实例的顶点/索引 `Arc` 指针相同。
- 真实流四个截断位置均报错。结构测试另外覆盖样本截断、越界值、格式/维度、非有限数值、缩放溢出、完整大矩形索引预算、60 字节小网格的总预算临界值及资源 ID 代际复用。
- 录制：`captures/physx-3.4-heightfield.pvdstream`，496386 字节，SHA-256 `1b19f6438f77887445bd47d7f61ff1b0c538cb85bcc36a906f48aee8e44d2083`。
- SDK 真值：`captures/physx-3.4-heightfield.heightfield.json`，856383 字节，SHA-256 `891a468ac84eb9dbce461f0556a677b471da44ab3a9cf309d18d4c22c3a26814`。重复运行的地址和计时会改变文件 hash。

此项运行证据限定 Windows x64 little-endian PhysX 3.4.2。4.1 和其他 SDK 的地形样例、运行中 `modifySamples` 的 PVD 更新行为、大型地形 GPU 压力、原版 PVD3 显示对照及 HeightField RepX 导出尚未验证或实现；不能由此推断原版保存容器兼容。
