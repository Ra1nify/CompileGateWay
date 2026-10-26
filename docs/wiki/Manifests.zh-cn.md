# 构建类型(manifest)

构建类型描述**一类工程该怎么构建**:靠哪些文件认出它、用户要填哪些值、在目标机上按顺序跑哪些命令。

## 什么是构建类型

一份构建类型就是一个 JSON 文件:`manifest/<id>.json`,和 `config.json` 放在一起。**文件名就是类型 id**(`manifest/xcode.json` → `xcode`),文件同时回答两件事:`include` 说明"这是不是我这类工程",`build` 说明"怎么把它构建出来"。

随程序附带八个内置类型:

| id | 显示名 | platform |
|---|---|---|
| `xcode` | Xcode (iOS) | `xcode` |
| `xcode-ipa` | Xcode (iOS Unsigned IPA) | `xcode` |
| `rust-native` | Rust (Cargo) | `rust` |
| `flutter-ios` | Flutter for iOS | `flutter` |
| `flutter-android` | Flutter for Android | `flutter` |
| `kotlin-multiplatform-ios` | Kotlin Multiplatform for iOS | `kotlin-multiplatform-ios` |
| `gradle-android` | Gradle (Android) | `gradle` |
| `cmake-native` | CMake (C/C++) | `cmake` |

这个目录**只在首次铺一次,之后就归你**:第一次启动时如果 `manifest/` 不存在,daemon 会把它建出来并把模板复制进去;此后不再插手 —— 你删掉的文件不会自己回来,你改过的内容也不会被覆盖。旧版本留下的 `variety/` 目录会在启动时**原样改名**为 `manifest/`,不复制、不丢文件。

WebUI 和 CLI 列表里的 `template: true` 只表示"这份和自带的某一份同名,所以能补齐",**不是特殊待遇**。

只有 `displayName` 和 `build` 是必填的,其余字段都可以省略并落到默认值。

## 字段参考

| 字段 | 类型 | 必填 | 含义 |
|---|---|---|---|
| `displayName` | string | 是(非空) | 列表和选择框里显示的名字。 |
| `platform` | string | 否 | 开放标签:`xcode` / `gradle` / `flutter` / `dart` / `rust` 或任意其它值。它**从不被校验**,所以补一个新平台不需要改代码。 |
| `include.paths` | string[] | 否(空数组永不命中) | 探测规则,相对工程根目录。**每一条都必须存在**才算命中。 |
| `variables` | object[] | 否 | 构建前由用户填的值。每项含 `name`(string,必填,唯一)、`label`(string,留空则退回 `name`)、`required`(bool)、`default`(string 或 null)。 |
| `build.steps` | array | **是**(非空) | 在目标机上按顺序执行的命令。每一项要么是命令字符串,要么是 `{ "id": "...", "run": "..." }`。 |
| `build.artifactPath` | string | 否 | 单个默认产物路径;`build.artifacts` 为空时用它。 |
| `build.artifacts` | string[] | 否 | 产物列表;非空时以它为准。 |
| `cache.paths` | string[] | 否 | 跨构建保留的远端目录。它们会参与推送排除集(见下)。 |

一份完整示例,逐项注解见后:

```json
{
  "displayName": "Xcode (iOS)",
  "platform": "xcode",
  "include": { "paths": ["*.xcodeproj"] },
  "variables": [
    { "name": "scheme", "label": "Xcode Scheme", "required": true },
    { "name": "configuration", "label": "Build Configuration", "required": false, "default": "Release" },
    { "name": "sdk", "label": "SDK", "required": false, "default": "iphoneos" }
  ],
  "build": {
    "steps": [
      {
        "id": "build",
        "run": "xcodebuild -scheme {{scheme}} -configuration {{configuration}} -sdk {{sdk}} -derivedDataPath .cgw/derived-data {{signingExtraArgs}} build"
      }
    ],
    "artifactPath": ".cgw/derived-data/Build/Products/{{configuration}}-{{sdk}}/{{scheme}}.app"
  },
  "cache": { "paths": [".cgw/", "Pods/"] }
}
```

注解:

- `displayName` 是 WebUI 卡片上的名字;`platform` 只是一个徽标。
- `include.paths` 只有一条规则,而且是末段通配:工程根目录下任意 `*.xcodeproj`。
- 三个 `variables` 由任务逐个填;`scheme` 必填,另两个留空就用默认值。
- 唯一的一步**有名字**,叫 `build`,所以构建日志能在失败时点名是它。
- `{{signingExtraArgs}}` 没有写在 `variables` 里 —— 它是唯一的保留占位符(见下)。
- `artifactPath` 是路径模板:同样做 `{{...}}` 替换,但不做 shell 转义。
- `cache.paths` 列出必须在多次构建之间保留的目录。

## `include.paths`:探测规则

**每一条都必须存在**才算这个目录命中该构建类型 —— 规则之间是"与",不是"或"。`include.paths` 为空的 manifest 永不命中任何目录。

不含 `*` 的规则就是拼出来的路径存在性判断(`gradlew`、`composeApp/src/iosMain`)。规则可以用 `*` 通配**最后一段**(`*.xcodeproj`、`iosApp/*.xcodeproj`):这时会列出父目录并按通配比较条目。开头的 `./` 会被忽略。

三个入口都提供探测,而且都针对**宿主机**(daemon 所在机器)上的目录:

- WebUI:构建类型页上有一个路径输入框加一个搜索按钮。
- CLI:

```bash
cgw manifests detect /path/to/project
```

- MCP:`cgw_detect_manifests` 工具。

daemon 返回**全部命中**,不替你挑一个;命中多份时由用户自己选。探测只认允许的宿主机根目录:路径越界返回 `403 outsideRoot`,路径不是目录返回 `400 sourceNotFound`。解析不通过的 manifest 会被跳过。

## `variables`

变量是任务传给一次构建的值。声明形式是 `{ "name", "label", "required", "default" }`;名字在同一份 manifest 内必须唯一,空名字会被拒。

- **替换**:步骤命令或产物路径里的 `{{name}}` 会被替换成变量的值。没声明的占位符在渲染时原样保留,但保存或加载这样的 manifest 会被拒 —— 除 `signingExtraArgs` 外,每个占位符都必须是已声明的变量。
- **取值顺序**:任务给的值 → 声明的 `default` → 空。值会做 trim;任务给的值若为空串,则退回 `default`。
- **必填**:标记 `required` 的变量最终为空时,构建以 `400 variableRequired` 失败,错误里会点名是哪个变量。
- **shell 转义**:在步骤命令里,替换后的值会先做 shell 转义再进 shell —— 只由字母、数字和 `/ . _ - ~ + : = ,` 组成的值原样通过,其余一律用单引号包起来,内部的单引号做转义。一个叫 `My App; rm -rf /` 的 scheme 会变成**一个参数**,而不是两条命令。产物路径**不做** shell 转义:它是路径,不是命令。

### 保留变量 `signingExtraArgs`

`signingExtraArgs` 是内置占位符,**不要**在 `variables` 里声明它(声明了就是校验错误)。它由任务的 `signingMode` 注入,展开成**多个裸参数**,所以它是被单独处理的、刻意不加引号的:

| `signingMode` | 展开结果 |
|---|---|
| `signed` | 空(什么都不加) |
| `unsigned` | `CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY=` |

除 `signed`(不分大小写)以外的任何取值都按 `unsigned` 处理。

## `build.steps`

每一步接受两种写法:

```json
"steps": [
  "flutter pub get",
  { "id": "pods", "run": "cd ios && pod install" }
]
```

- 纯命令字符串 —— 最早的写法。
- 命名步骤 `{ "id": "...", "run": "...", "outputs": { ... } }`。`id` 必须非空且在整份 manifest 内唯一。
  - **诊断显示**: 构建失败时远端往往只吐一句裸的工具报错，只要有任一步带名字，daemon 就会先写一行列出各步(`id(1) -> id(2) -> ...`)，精准定位失败步骤。
  - **步骤级产物声明 (`outputs`)**: 命名步骤可声明自身产出的产物路径，支持键值映射或对象列表（可选字段包括 `path`、`description`、`required` 默认为 true、`directory` 默认为 false）。构建成功后 daemon 会自动验证其存在性与目录/文件类型；并在批量工作流中将产物注册到 `job.produced`，供后续步骤以 `{{steps.<stepId>.outputs.<name>}}` 消费。
- 命令不能为空;空命令是校验错误。
- 所有步骤**按顺序用 `&&` 串成一条命令**:`cd <远端路径> && step1 && step2 && ...`。**第一个非零退出就停**,整次构建是一个 SSH exec。

## 产物

产物是这次构建应该产出的东西。两个字段都是模板,会用任务的变量渲染(不做 shell 转义):

```
任务的 artifacts  >  任务的 artifactPath  >  manifest 的 build.artifacts  >  manifest 的 artifactPath
```

- 同一层里,非空列表压过单一路径;空白项会被丢掉。
- 任务可以逐任务覆盖 manifest:先看任务的 `artifacts`(非空),再看任务的 `artifactPath`,然后才是 manifest 自己的列表,最后是它的单一路径。
- 相对路径按远端工程目录解析;以 `/` 或 `~` 开头的路径原样使用。
- 构建成功后 daemon 会重新连上去逐个确认产物是否存在,报出大小,并在 job 消息里点名缺失的那些。
- 产物也是 daemon 在 job 里报告的内容:job 记录带 `artifactPath`(第一个产物)和完整的 `artifacts` 列表。

## `cache.paths`

构建类型在这里声明**跨构建要保留的远端目录**,例如 `.gradle/`、`ios/Pods/`、`.cgw/`。

这些路径会并入推送时使用的排除集,所以宿主机上的同名缓存目录不会覆盖远端的。一次推送排除:

- 项目自己的排除项,默认是 `.git`、`.DS_Store`、`Thumbs.db`;
- 固定的"宿主机本地产物"黑名单(比如 `DerivedData`、`.cgw`、`build`、`.gradle`、`node_modules`、`.dart_tool`、`Pods`、`target` —— 这些**永远不推送**);
- 该项目用到的**每一份** manifest 的 `cache.paths`。

排除项按"名字相等"(任意深度,`.git` 也能匹配 `sub/.git`)或"含 `/` 时按相对路径前缀"匹配(`ios/Pods/` 覆盖它下面的一切)。

## 管理构建类型

CLI 覆盖全部操作:

```bash
cgw manifests                      # 列出(等同 `manifests list`)
cgw manifests status               # 目录在哪、共几份、哪几份不合法、缺哪些模板
cgw manifests show <id>            # 打印原始 JSON
cgw manifests save <id> --file <路径>   # 或 --file - 从标准输入读;覆盖已有 id 需要 --force
cgw manifests rm <id>
cgw manifests templates            # 还缺哪几份自带模板
cgw manifests reset [id...]        # 只写回**不存在**的模板文件
cgw manifests refresh              # 从磁盘重读,报告新增 / 改动 / 删除
cgw manifests detect <宿主机目录>
cgw manifests import <文件.json> [--id <名称>] [--force]
cgw manifests export <id> [--save <路径>]   # 不给 --save 就打到标准输出
cgw manifests schema               # 等同 `cgw schema manifest`
```

几点说明:

- `reset` 只写不存在的文件,绝不碰你改过的那份;想拿回原版模板,先删掉它再补齐。
- `import` 和 `save` 覆盖已有 id 时必须显式 `--force`,脚本无法静默冲掉你的构建类型。
- `refresh` 是相对**本次运行中上一次看到的状态**做差异(启动时先记一次基线),所以第一次刷新不会把四份模板全报成"新增"。把整个目录删掉再刷新,会重建目录并重新铺模板。
- **WebUI 的构建类型页**做同样的事:工具栏有刷新、"补齐缺失的模板"(只在确实有缺失时出现)和新建;一个探测输入框加运行按钮;每张卡片上可以编辑原始 JSON、导出文件、删除。因为浏览器读不到用户选中的本地文件,"新建"只问一个 id 并写入一份起始 manifest —— 要从磁盘带进一份文件请用 `cgw manifests import`。
- manifest id 只允许字母、数字、`-` 和 `_`,保存接口会拒绝其它字符。保存一份校验不通过的 manifest 会返回 `400`,并附上完整的校验信息列表。

## JSON Schema

每份构建类型都要满足一份 JSON Schema,而这份 schema 是**从 Rust 结构体生成**的 —— 它不可能和实现脱节:

```bash
cgw schema manifest
```

同一份 schema 还可以通过 MCP 资源 `cgw://schema/manifest` 取得,或走 HTTP:`GET /api/schema/manifest`。它是 draft-07 schema,写明的约束和代码真正拒绝的东西一致:`displayName` 与 `build` 必填、`build.steps` 至少一项、`variables[].name` 非空、`default` 是可空字符串、`steps` 是两种步骤写法的 `anyOf`。

## 两个完整例子

### 自带的 `xcode` manifest

逐字取自 `src-tauri/templates/xcode.json`,其中是一步命名步骤:

```json
{
  "displayName": "Xcode (iOS)",
  "platform": "xcode",
  "include": { "paths": ["*.xcodeproj"] },
  "variables": [
    { "name": "scheme", "label": "Xcode Scheme", "required": true },
    { "name": "configuration", "label": "Build Configuration", "required": false, "default": "Release" },
    { "name": "sdk", "label": "SDK", "required": false, "default": "iphoneos" }
  ],
  "build": {
    "steps": [
      {
        "id": "build",
        "run": "xcodebuild -scheme {{scheme}} -configuration {{configuration}} -sdk {{sdk}} -derivedDataPath .cgw/derived-data {{signingExtraArgs}} build"
      }
    ],
    "artifactPath": ".cgw/derived-data/Build/Products/{{configuration}}-{{sdk}}/{{scheme}}.app"
  },
  "cache": { "paths": [".cgw/", "Pods/"] }
}
```

### 一份最小的两步 manifest

两个无名步骤,没有变量,没有缓存:

```json
{
  "displayName": "Make project",
  "platform": "make",
  "include": { "paths": ["Makefile"] },
  "build": {
    "steps": ["make clean", "make all"],
    "artifactPath": "build/app.bin"
  }
}
```

用 `cgw manifests save make --file make.json` 保存之后,它就能直接在 `build` 任务里选用了。构建类型由任务使用:`manifest`、`variables`、`signingMode` 和任务级产物覆盖见 [任务](Tasks.zh-cn),完整命令面见 [命令行](CLI.zh-cn)。

← [首页](Home.zh-cn)
