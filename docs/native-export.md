# 原版容器保存与裁剪

`pvd_core::native::export` 支持受限的 PVD3 数据库 v1 重写和帧区间裁剪。桌面“时间轴”和“片段”页提供整份另存与片段保存，CLI 提供 `native-save`、`native-trim`。这些入口接收原版容器，不把 SDK 流转换成原版文件。

整份保存逐条核验结构与事件重编码结果，以有界分块 I/O 保留完整源字节，并复核源身份。前缀导出需要重建目录和保存实例位置；内部恢复与回放检查不替代 PVD3 实际打开、正倒放和重存，验收边界见路线图 `FILE-01`、`FILE-02`。

整份保存可保留已识别的 type 11 OriginShift 事件，包括位移的原始浮点位模式及辅助实例/属性对。行数、完整长度、尾部消费和有限位移均需通过检查，页、快照、backing 和 padding 保持源字节。该事件的整份保存及受限帧裁剪有静态格式证据和构造回归，尚无包含该事件的真实原版配对。

```powershell
cargo run --locked -p pvd-cli -- native-save captures/native/rich-paired-native.pxd2 captures/native-copy.pxd2
cargo run --locked -p pvd-cli -- native-trim captures/native/rich-paired-native.pxd2 0 59 captures/native-prefix.pxd2
```

输出使用 `.pxd2`，已有目标不覆盖；父目录应先准备好。桌面选择路径或保存时会创建所需父目录。

## 前缀范围

空 event=0 快照的前缀裁剪支持多个 Scene，按 frame section 的结束事件顺序选择末帧。截点必须同时位于所有 Scene 的完整帧边界；若另一个 Scene 的帧已经开始但尚未结束，则明确拒绝。每个 Scene source 只能对应一个生命周期，section 必须处于其创建与销毁之间。多 Scene 范围由构造回归覆盖，真实原版多 Scene 配对仍待补齐。

非空 event=0 快照支持普通多 Scene 的前缀再裁剪，各 Scene 同样要求完整帧边界。初始普通对象及错误数组保留原字节，快照与销毁时的保存体按位置分别处理；源事件号、撤销引用和错误接收位置不变。起点已有性能或非空绘制缓冲时，仅接受下文的性能、内存和绘制组合范围；其他绘制起点须为空，首个非空缓冲须显式设置绘制实例，未知调试器分析状态仍拒绝。快照引用必须指向起点存活对象，实例 ID 不得复用。起点之后才由真实 Create 创建的性能 Zone 沿常规事件路径处理，不要求起点分析授权。此范围通过构造回归，以及真实 baseline、rich 派生片段与同次 SDK 的逐帧对照；派生片段不是原版实际保存的非空快照。

起点内存支持一对已识别的事件缓冲与来源总量对象：保留快照原始数组、完整来源目录和区间内实际刷新，包括有符号总量及合法销毁时的保存体。旧缓冲只作为属性保留，不重播为新区间事件；总量不用于推算分配地址或累计前史。重复对象、ID 复用、数组编辑、属性消息及未知布局拒绝。内存快照、目录、刷新与撤销引用另作独立字节核验；此范围有手写容器覆盖，真实原版非空内存起点仍待配对。

两类前缀都要求唯一 event=0 快照及已支持布局。具名相机的前缀、中段和非空起点再裁剪使用独立范围，见[具名相机裁剪](#具名相机裁剪)。

含 OriginShift 的前缀限空起点，保留区间不能有 type 10 保存实例或对象销毁。场景成员限静态和动态刚体；Scene 成员列表与 Actor.Scene 必须相互对应，辅助行须覆盖目标场景中已定义的 GlobalPose、WorldBounds 和 KinematicTarget。属性所属、类型、宽度和存储范围须通过检查；保持原始事件号、浮点位模式、行顺序及撤销引用，不重算属性字节。非空起点及其他成员类型仍拒绝。

平移字段接受定长叶布局及标准复合布局：`PxTransform` 的 `q/p`、`PxBounds3` 的 `minimum/maximum`，内部 `PxQuat/PxVec3` 只接受标准浮点成员。标准复合布局的内置类型、成员顺序、32 位存储偏移与 packing 须一致，未知复合布局拒绝。这组结构已由真实原版元数据与固定 SDK 核对，含 type 11 的裁剪行为仍属构造验证范围。

写出需重建容器头、通信目录、帧区间、事件页和索引，并重新定位 type-10 保存实例与 backing 数据。帧尾后的分析事件不能移动到最后一帧，未知元数据只能在已确认不需要修改的范围内保留。

## 中段裁剪边界

非零起点按所有 Scene 的 Section End 事件顺序选择观察帧，通过全局快照和撤销链重建。起点取首个保留帧组中最早的 Begin；前一个排除帧必须已经结束，末端不得截断其他 Scene 的帧。各 Scene 保留自己的来源和时间戳，不按不同来源的时间戳重新排序。快照使用捕获的原始属性字节，以及原版 Create 定义的已知类型初始化值；跨起点撤销先恢复基线，再应用区间内更新，并逐帧比较输出状态。快照对象体按存储位置区分，存活基线与销毁时的对象体可以属于同一对象。

已有非空 event=0 快照的片段支持受限中段再裁剪。起点须含存活 Scene，普通标量、继承字段、固定布局数组和只追加错误按捕获历史恢复；未写字段、数组头及填充保留初始保存体的原始字节。区间内保存体可以引用在该片段中已创建或起点存活、随后销毁的对象；普通属性更新仍要求引用目标当时存活。起点前已销毁、尚未创建及未知对象不能作为新区间保存体的引用目标。OriginShift 及未知调试器布局仍不在此范围内；具名相机另按下述范围核验。

初始内存对象对的中段再裁剪支持已闭合的多 Scene 帧：按准确保存位置取得原始固定区和数组，后续真实完整写入才替换属性。无前置刷新、仅总量更新、空缓冲及空总量均保留各自含义；初始属性的首次撤销可指向真正的起点标记，后续撤销须指向同一属性的前次完整写入。不支持内存对象的局部编辑、保存实例、销毁或重建。该范围使用手写容器验证，真实原版非空内存起点仍待配对。

单 Scene 的性能、内存和绘制起点可在前缀及中段中组合：一个或多个稳定性能 Zone，与实例来源目录一致，共用同一个已识别的 24 字节缓冲类。初始性能属性为空，每个 Zone 须有一条首帧前真实存在、仅含完整绝对 Start 的缓冲，这些记录从 event 1 连续排列。身份、事件目录、配对栈与撤销均按 Zone 独立检查，同名和同事件号不合并。初始绘制缓冲可非空，但只保留为属性；源首个及区间首个实际绘制缓冲必须自行设置实例，不重播旧图元或从中恢复上下文。性能与绘制对象在全源范围内不得创建、销毁、局部修改或保存实例；目录、原始属性、保留事件及撤销关系独立核验。

前缀保留原初始快照及截至末帧的事件，包括已有 Start 起点记录；不生成新起点，事件号、撤销引用和错误接收位置不变，仅重排保存实例的物理位置。中段截点的各区性能属性须为空，未完成起点由实际源事件重建，并按新增起点记录数重定位保留事件和撤销引用。某区无未结束事件时不补造 Start，其输出暂不满足再次裁剪所需的每区起点条件。真实 rich 单区派生片段的前缀、中段及连续组合已与源区间、直接裁剪结果和同次 SDK 对照；多区范围有独立构造专项，仍缺真实原版多区配对。

OriginShift 中段支持闭合的多 Scene 帧，沿用上述刚体布局，要求源快照为空，且截至所选末帧没有保存实例。来源与 Scene 必须唯一对应。起点前的平移按捕获顺序作用于各 Scene 的快照属性；区间内平移必须能够通过反向位移恢复每个 `f32` 的原始位模式，舍入或符号零丢失时拒绝。属性撤销若引用任何 type 11 事件也拒绝。每个在起点前已有平移的 Scene，均须在首个保留的全局 End 之前包含自己的真实平移事件，以恢复场景平移标记；不能补造事件。写出仅重定位事件号，位移、辅助行及顺序保持原字节，并单独核对摘要。

原版 Create 对固定存储清零，数组头对应空数组。导入仅在类型、命名空间、字段宽度和偏移已识别时恢复这种确定初值；未知布局保持未知。写出另用实际保存对象体核验未写字段与生命周期，不用未来字段值替代起点状态。这类原版初始化值与 SDK 尚未发送的属性分别核对。

继承字段必须落在具体类的固定存储内，且不能重叠；非空引用、未知复合类型和动态嵌套布局不由初始化规则补全。普通读取只按元数据建立初值，不扫描未来事件。写出的独立保存体核验按对象生命周期检查属性、属性消息及数组写入；复用 ID 或多个保存版本不能混作同一个起点证据。

固定布局数组的撤销从当前事件执行后的值开始，按捕获引用的顺序恢复，再与事件前的整个数组逐字节比较。来源可组合起点快照、Create 的空数组、完整属性写入和保留区间的插入/元素更新。引用必须严格向前、按旧到新排列，并对应同一对象与属性；删除来源按原版访问器执行空操作，不递归其撤销。跨起点引用折叠为快照后仍须通过完整状态核验，不能只比较被编辑的元素。

撤销元素删除或更新时，来源事件使用当前编辑的索引；撤销完整数组赋值时，各来源使用各自的索引，数组不会预先清空。回溯插入的负索引或超长索引按原版规则追加，回溯元素更新的越界索引不写入。原始正向编辑仍要求已支持的单元素宽度与有效索引。同对象的属性消息若没有匹配当前数组的成员，不写入数组；扫描仍受工作预算和取消检查约束。未知布局及不完整的恢复链拒绝。

属性消息的撤销使用独立规则：从消息写入后的实际字段值恢复；历史数组插入、删除和元素更新只消费记录，不修改字段。完整属性来源和消息成员按捕获顺序应用，最后比较全部目标字段，不能由空值起算或隐式清空。重复消息成员按元数据顺序执行。

前缀和中段裁剪均要求属性消息只写标量字段，核对来源与目标的类型 ID、名称、固定宽度、目标宽度及消息边界。消息声明的所属类须与类表一致，并且是目标对象的本类或祖先类；缺失、循环或超过 128 层的继承链明确报错。

不同类型接受已配对的 `String→StringHandle`、`VoidPtr→ObjectRef` 存储映射，并检查引用与字符串有效性。按位别名支持同位宽 I/U8、I/U16、I/U32、I/U64 及 Vec4/Quat 各自双向；Bool 只允许作为来源写入 I8/U8，且实际字节须为 0 或 1，反向仍拒绝。各方向核对固定内置 ID、名称、packing、无指针及精确成员布局，保留原始字节；依据是原版静态存储规则和独立构造回归，尚无这些方向的真实容器互验。同宽不表示一般数值转换成立。数组属性在固定存储中是数组头，不能将消息成员解释为完整数组，因此帧裁剪仍拒绝数组目标消息；整份字节保存不表示数组消息语义已验收。

事件历史按各自的捕获语义保留：

- 性能事件保留区间内的完成记录，以及跨起点仍未结束的真实 Start。起点保留原始 zone、event、thread、context、tick、CPU 和优先级，并以完整字段编码恢复；不重播起点前已完成的记录。源缓冲独立解析出的配对栈须与分析器一致；受影响的属性必须在首个保留帧内由真实缓冲覆盖，避免起点种子成为该帧的属性值。完成事件、值和末尾未结束起点均须与源区间一致。
- 内存起点可保留事件缓冲和来源总量的原始快照或最后一次真实完整写入。多 Scene 共用唯一已知内存对象对，按全局 Section End 事件序保留刷新；截至所选末帧不支持这些对象的保存实例、销毁、局部编辑或重建，也不与 OriginShift 组合。上一帧结束至首个保留帧开始之间不得有内存刷新，包括空刷新，避免丢失首帧事件。起点旧缓冲只作属性保存；来源目录、总量、实际区间事件、帧边界及撤销关系独立核验。地址、峰值和累计量仅从区间内实际事件计算，起点前分配的释放可显示为未知，不补造前史；完整内存 CSV 明确拒绝。
- 空源快照的绘制中段可恢复此前真实设置的绘制实例。仅当保留图元在首次真实设置之前依赖该上下文时，加入只含实例设置的起点记录；该属性必须在首个保留帧被真实缓冲覆盖，不重播旧图元。支持已识别的 12/24 字节 renderer 数组头；检查完整缓冲、撤销链及区间结束时存活的来源映射。前一帧结束至选段开始间有图元时拒绝，与 OriginShift 的上下文组合另有限制。每帧绘制结果须与源区间严格相等。
- 错误保留区间内的追加记录，保留来源 Index、文本和顺序，并重定位事件号及接收帧。起点仍存活的错误容器，其已有数组随快照保留，另列为“起点已有错误”，不重播为新的发生记录。校验按源事件序号和独立跟踪的存活容器核对各容器内容、顺序及重复记录；不从接收帧猜测截点归属，不补造更早接收位置。替换或编辑错误数组时拒绝。

快照不能代替性能配对栈、存活分配表或错误发生顺序。性能历史只重建未结束起点，不移动起点前已完成的缓冲。未知事件历史操作和未验证的数组撤销仍拒绝。真实 baseline 中段、rich 的三个短区间及关节限位的三个区间已通过源区间、同次 SDK 和正倒放状态对照；内存事件按区间重新折叠核对，来源总量与源帧逐行比较。这不代表其他布局、完整前史或 PVD3 打开、正倒放和重存已验收。

## 具名相机裁剪

具名相机支持完整多 Scene 帧边界的前缀、中段及派生非空起点再裁剪。对象限已知 `Camera` 的 40 字节压紧布局：Name 为 StringHandle，Origin、Up、Target 为 PxVec3；PxVec3 可为已知 12 字节叶类型或规范 x/y/z 三个 F32 成员。属性更新须为已知标量或完整 36 字节向量消息。存储按 32 位布局读取，不要求原始 64 位字段偏移相同。

前缀保留原快照与事件；中段起点取首个保留 Begin 之前的原始属性，包括帧间更新和销毁。目录保留起点存活或区间内创建的实例，包含区间内随后销毁的相机；起点前已销毁及末帧后才创建的项不保留。名称和浮点位模式原样保存，不从最终目录补造历史。

完整源的布局、目录和生命周期须有效。起点、保存体与销毁保存体分别核对原始 40 字节；源撤销先按真实来源恢复并比较更新前字段，跨截点引用才可折叠到起点标记，不能指向其他分析起点记录。输出另核对目录、起点、历史与每帧对象和相机状态，首帧完整更新不能掩盖错误基线。

相机不具有捕获的 Scene 归属，共享引用不作为归属依据。裁剪核验保留原始 Section 来源、时间戳、事件顺序和时钟比例。相机与稳定单区或多区性能起点的组合仍限单 Scene，内存和绘制继续遵守各自限制。多 Scene 相机与共享内存组合的前缀、中段及再次裁剪已有构造验证，包含空/非空起点、相机销毁、原始撤销引用、内存来源总量及磁盘正倒/随机恢复；不代表原版应用互通。源相机及所选区间、重建输出须分别通过核验，性能起点只从源事件恢复。预算覆盖共存的相机、分析核验状态与临时索引。OriginShift、未知布局、缺少各 Zone 真实 Start 或非空初始性能属性的组合仍拒绝。

上述相机范围及其性能组合已有静态格式依据和构造验证；原版保存的非空相机目录及其性能组合仍缺真实配对。读取与测试入口见[相机和显示属性](native-settings.md)。

## 完整性要求

裁剪的保存实例检查固定布局、继承字段、数组、ObjectRef/StringHandle 及 packed 引用，并与该时刻已传输属性及已识别的 Create 初值比较。普通正向读取跳过的辅助对象体也须有效；仅用正向自读无法验证全部撤销及快照引用。整份保存验证布局和字节一致性，不额外承诺完整历史语义已通过。

必须拒绝越界撤销引用、缺少区间生命周期依据的引用、重复 local ID 和与事件状态不符的保存实例。前缀保留源撤销引用并检查结构边界，不据此保证源文件的全部撤销语义；中段还须重建并比较所支持布局的撤销结果。最终名称与显示目录只代表保存时状态，不能补造缺失的变更历史。

整份保存不读入完整文件，单次 I/O 不超过 64 KiB；分块摘要索引限 64 MiB，元数据及结构索引各受预算限制，历史最多两千万事件。开始时计算完整源身份，读取时逐块校验，写完后再次检查，因此会多次扫描源文件。该路径支持超过 512 MiB 的输入；真实大型原版录制仍需独立验收。

帧裁剪的输入和输出各不超过 512 MiB，元数据及辅助索引有独立预算；裁剪期存活对象的类索引限 64 MiB，前缀重排与核验各使用 64 MiB 辅助索引预算。中段起点的原始属性状态与生成快照分别限制为 64 MiB，每份分析核验状态各受 128 MiB 预算限制。数组撤销使用借用片段表示中间值，不复制每个中间数组；中间值和片段索引各限 64 MiB，单次导出的核验工作额度为 1 GiB，按比较及片段处理量计量，长比较每 64 KiB 检查取消。核验摘要直接流式计算，不生成额外序列化副本；消息字段、继承链及性能/内存解析均检查取消。帧裁剪会读入完整输入并规划输出，文件上限不是进程总内存保证；字节切片重写 API 也保留该上限。

CLI 与桌面使用[无覆盖输出](cli-output.md)：结果先写入临时文件，复核完整源摘要、长度及载入身份，完成同步和取消检查后才发布。Windows 文件源在导出期间只允许其他读取者打开。核心 API 的调用者须自行暂存输出，失败或取消可能已向 sink 写入部分字节。

## 验证

构造及普通核心回归不需要额外捕获，夹具生成器保持忽略：

```powershell
cargo test --locked -p pvd-core --lib native::export -- --test-threads=1
```

多 Scene 构造专项覆盖交错帧、共享引用、帧间更新、Scene 销毁、空/非空起点及连续裁剪，并核对保存体与显式预期状态。设置 `PVD_NATIVE_MULTI_SCENE_DIRECTORY` 后，显式运行 `retain_multi_scene_trim_fixtures` 可留存输入与结果。各来源时间戳不构成统一物理时钟，完整帧边界也不表示各 Scene 同时模拟。

超过 512 MiB 的流式保存与源变化检查：

```powershell
cargo test --locked -p pvd-core --lib whole_save_above_512_mib -- --ignored --nocapture
cargo test --locked -p pvd-core --lib whole_streamed_save_matches_original -- --ignored --nocapture
cargo test --locked -p pvd-core --lib reader_rechecks_source_content -- --ignored --nocapture
```

首项使用构造读取器，其余两项需要下面的原版输入目录。桌面专项使用追加零 padding 的实际文件，检查后台打开、整份另存、裁剪禁用、源变化拒绝发布和关闭后的历史回收：

```powershell
New-Item -ItemType Directory -Path output/native-large -Force
$env:PVD_NATIVE_LARGE_FILE = Join-Path $PWD 'output/native-large/baseline-padded.pxd2'
[IO.File]::Copy((Resolve-Path 'captures/native/baseline-paired-native.pxd2').Path, $env:PVD_NATIVE_LARGE_FILE, $false)
$fixture = [IO.File]::OpenWrite($env:PVD_NATIVE_LARGE_FILE)
try { $fixture.SetLength((513MB) + 19) } finally { $fixture.Dispose() }
cargo test --locked -p pvd-desktop studio::native_export::tests::padded_native_file_above_512_mib_keeps_identity_and_whole_save_workflow -- --ignored --exact --nocapture --test-threads=1
```

目标文件须不存在。该专项仅扩展文件体积，不增加事件或对象数量；它不是大型真实录制、长期资源趋势或原版应用互通验收。

真实输入按[原版验证](native-validation.md)准备，显式运行配对专项：

```powershell
$env:PVD_NATIVE_FIXTURES = (Resolve-Path 'captures/native').Path
$env:PVD_NATIVE_MEMORY_DIRECTORY = $env:PVD_NATIVE_FIXTURES
$env:PVD_NATIVE_ORIGIN_METADATA_DIRECTORY = $env:PVD_NATIVE_FIXTURES
$env:PVD_NATIVE_EXPORT_DIRECTORY = Join-Path $PWD 'output/native-export'
cargo test --locked -p pvd-core --test native_export actual_native_ -- --ignored --nocapture
cargo test --locked -p pvd-core --test native_memory_middle -- --ignored --nocapture
cargo test --locked -p pvd-core --lib native::export::origin_shift::layout_tests::original_metadata_matches_the_canonical_shift_layout -- --ignored --exact
cargo test --locked -p pvd-desktop recording_export::tests::native_exports_preserve_source_identity_cancellation_and_existing_outputs -- --ignored --exact --test-threads=1
```

核心配对专项比较三份原版容器的整份字节重建、前缀及中段的正倒序状态、几何、分析和同次 SDK 真值。核心输入目录可通过上述变量替换；桌面专项固定读取 `captures/native/baseline-paired-native.pxd2`。未知布局和原版应用互验仍须独立记录。

再次裁剪专项从真实 baseline 生成非空起点片段，再截取其前缀和中段，与源区间及同次 SDK 对照正倒放和随机定位，并核对整份字节保存。构造专项另覆盖起点错误、内存快照、双版本保存体、非零快照填充、绘制上下文、OriginShift 中段、损坏引用、取消与预算。设置 `PVD_NATIVE_MEMORY_PREFIX_FIXTURE_DIRECTORY` 后，`native::export::initial::memory::tests` 的忽略测试可留存手写内存前缀容器。

rich 派生片段专项独立解析原始事件和性能配对栈，核对前缀、中段及连续组合、区间内存事件、起点及结束总量、未完成性能事件和正倒/随机恢复。沿用上述同次配对输入目录，保留输出并运行：

```powershell
$env:PVD_NATIVE_INITIAL_ANALYSIS_MIDDLE_DIRECTORY = Join-Path $PWD 'output/native-export/rich-middle'
$env:PVD_NATIVE_INITIAL_ANALYSIS_PREFIX_DIRECTORY = Join-Path $PWD 'output/native-export/rich-prefix'
cargo test --locked -p pvd-core --test native_export native_initial_analysis_middle -- --ignored --nocapture
```

对应的手写组合容器由独立生成器留存，不需要真实输入：

```powershell
$env:PVD_NATIVE_INITIAL_ANALYSIS_DIRECTORY = Join-Path $PWD 'output/native-export/constructed-middle'
$env:PVD_NATIVE_INITIAL_ANALYSIS_PREFIX_DIRECTORY = Join-Path $PWD 'output/native-export/constructed-prefix'
$env:PVD_NATIVE_MULTIZONE_INITIAL_DIRECTORY = Join-Path $PWD 'output/native-export/constructed-multizone'
$env:PVD_NATIVE_CONSTRUCTED_EXPORT_DIRECTORY = Join-Path $PWD 'output/native-export/constructed-cameras'
cargo test --locked -p pvd-core --lib native::export::middle::initial_source::tests::initial_analysis -- --ignored --test-threads=1
```

构造与真实配对结果分别记录，两者均不替代原版保存非空起点或原版应用互验。

按位别名的边界位模式、本类/继承、重复及重叠成员、跨消息撤销、损坏元数据和取消由 `native::export::middle::tests::alias_tests` 覆盖。设置尚不存在的 `PVD_NATIVE_BITCOPY_ALIAS_DIRECTORY` 后，显式运行其忽略测试 `retain_bit_copy_alias_fixtures` 可留存八个整数方向的源及中段构造容器；父目录须已存在。

设置 `PVD_NATIVE_INITIAL_MEMORY_MIDDLE_DIRECTORY` 后，`native::export::middle::initial_source::tests::initial_memory::retain_initial_memory_middle_fixtures` 可留存初始内存中段输入及输出。该测试须加 `--ignored` 显式执行；它不替代真实原版或原版应用互验。

容器结构依据见[原版文件格式](original-file-format.md)，真实多页和非空起点仍需独立配对输入。
