# Build types (manifests)

A build type describes how one kind of project is built: which files identify it, what the user fills in, and which shell commands run on the target machine.

## What a build type is

A build type is one JSON file at `manifest/<id>.json`, created next to `config.json`. The **file name is the type id** (`manifest/xcode.json` → `xcode`), and the file carries two answers: `include` says "is this project of my kind", `build` says "how do I build it".

Eight types ship with the binary:

| id | display name | platform |
|---|---|---|
| `xcode` | Xcode (iOS) | `xcode` |
| `xcode-ipa` | Xcode (iOS Unsigned IPA) | `xcode` |
| `rust-native` | Rust (Cargo) | `rust` |
| `flutter-ios` | Flutter for iOS | `flutter` |
| `flutter-android` | Flutter for Android | `flutter` |
| `kotlin-multiplatform-ios` | Kotlin Multiplatform for iOS | `kotlin-multiplatform-ios` |
| `gradle-android` | Gradle (Android) | `gradle` |
| `cmake-native` | CMake (C/C++) | `cmake` |

The directory is **seeded once and then yours**. On first launch, if `manifest/` does not exist, the daemon creates it and copies the templates in. It never does that again: deleting a file does not bring it back, and editing one is never overwritten. A pre-existing `variety/` directory is renamed in place to `manifest/` at startup, so nothing is copied or lost.

`template: true` in the WebUI and CLI list only means "this file has the same name as a shipped template, so it can be restored". It is not special treatment.

Only `displayName` and `build` are required. Everything else may be omitted and falls back to a default.

## Field reference

| Field | Type | Required | Meaning |
|---|---|---|---|
| `displayName` | string | yes (non-empty) | Name shown in lists and pickers. |
| `platform` | string | no | Open label: `xcode` / `gradle` / `flutter` / `dart` / `rust` and anything else. It is never validated, so a new platform needs no code change. |
| `include.paths` | string[] | no (empty never matches) | Detection rules, relative to the project root. Every entry must exist for the directory to match. |
| `variables` | object[] | no | Values the user fills in before a build. Each entry has `name` (string, required, unique), `label` (string, falls back to `name`), `required` (bool), `default` (string or null). |
| `build.steps` | array | **yes** (non-empty) | Commands run in order on the target. Each entry is either a plain command string or `{ "id": "...", "run": "..." }`. |
| `build.artifactPath` | string | no | Single default artifact path. Used when `build.artifacts` is empty. |
| `build.artifacts` | string[] | no | Artifact list; wins over `artifactPath` when non-empty. |
| `cache.paths` | string[] | no | Remote directories kept across builds. They take part in the push exclude set (see below). |

A complete example, annotated below:

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

Annotations:

- `displayName` is what the WebUI shows on the card; `platform` is only a badge.
- `include.paths` holds one rule, a last-segment wildcard: any `*.xcodeproj` at the project root.
- The three `variables` are filled in per task; `scheme` is required, the other two fall back to their defaults.
- The single step is **named** `build`, so the build log can name it on failure.
- `{{signingExtraArgs}}` is not declared in `variables` — it is the one reserved placeholder (see below).
- `artifactPath` is a path template: the same `{{...}}` substitution applies, but no shell quoting.
- `cache.paths` names the directories that must survive between builds.

## `include.paths` as detection rules

Every entry in `include.paths` must exist for a directory to match that build type — the rules are ANDed, not ORed. A manifest with an empty `include.paths` never matches anything.

A rule with no `*` is a plain existence check on the joined path (`gradlew`, `composeApp/src/iosMain`). A rule may use `*` in its **last segment** (`*.xcodeproj`, `iosApp/*.xcodeproj`); the parent directory is listed and its entries are wildcard-matched. A leading `./` is ignored.

Detection is offered by all three front ends, always against a **host** directory (the machine the daemon runs on):

- WebUI: the build-type page has a path box plus a search button.
- CLI:

```bash
cgw manifests detect /path/to/project
```

- MCP: the `cgw_detect_manifests` tool.

The daemon returns every match, not one winner — when several types hit, the user picks. Detection only sees directories inside the allowed host roots: a path outside them is rejected with `403 outsideRoot`, a path that is not a directory with `400 sourceNotFound`. Manifests that fail to parse are skipped.

## `variables`

Variables are the values a task supplies to a build. Each is declared as `{ "name", "label", "required", "default" }`; names must be unique per manifest, and the empty name is rejected.

- **Substitution**: `{{name}}` in a step command or in an artifact path is replaced by the variable's value. An undeclared placeholder is left verbatim by the renderer, but saving or loading such a manifest is rejected — every placeholder must be a declared variable, except `signingExtraArgs`.
- **Resolution order**: the value given by the task, else the declared `default`, else empty. Values are trimmed; an empty provided value falls back to the default.
- **Required**: a `required` variable that ends up empty fails the build with `400 variableRequired`, and the error names the variable.
- **Shell quoting**: in step commands a substituted value is shell-quoted before it reaches the shell — values made only of letters, digits and `/ . _ - ~ + : = ,` pass through, anything else is wrapped in single quotes with embedded quotes escaped. A scheme named `My App; rm -rf /` becomes one argument, not two commands. Artifact paths are **not** shell-quoted: they are paths, not commands.

### The reserved `signingExtraArgs`

`signingExtraArgs` is a built-in placeholder and must **not** be declared in `variables` (declaring it is a validation error). It is injected from the task's `signingMode` and expands to **several bare arguments**, so it is deliberately never quoted as a single value:

| `signingMode` | expansion |
|---|---|
| `signed` | empty (nothing is added) |
| `unsigned` | `CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY=` |

Any value other than `signed` (case-insensitive) is treated as `unsigned`.

## `build.steps`

Two forms are accepted for each step:

```json
"steps": [
  "flutter pub get",
  { "id": "pods", "run": "cd ios && pod install" }
]
```

- A plain command string — the original shape.
- A named step `{ "id": "...", "run": "...", "outputs": { ... } }`. The `id` must be non-empty and unique within the manifest.
  - **Diagnostic display**: On a build failure the remote prints a bare tool error, so the daemon first writes a line listing the steps (`id(1) -> id(2) -> ...`) whenever any step is named, identifying the failing step precisely.
  - **Step-level output declaration (`outputs`)**: Named steps can declare outputs produced by that step, supporting key-value mappings or an array of objects (optional fields include `path`, `description`, `required` defaulting to true, `directory` defaulting to false). Upon successful build completion, the daemon verifies their existence and directory/file types. For batch workflows, these outputs are registered into `job.produced` for downstream steps to consume via `{{steps.<stepId>.outputs.<name>}}`.
- Commands must be non-empty; an empty command is a validation error.
- The steps run **in order, joined by `&&`** into one remote command: `cd <remotePath> && step1 && step2 && ...`. The first non-zero exit stops the build, and the whole run is one SSH exec.

## Artifacts

An artifact is what the build is supposed to produce. Both fields are templates and are rendered with the task's variables (no shell quoting):

```
task.artifacts  >  task.artifactPath  >  manifest.build.artifacts  >  manifest.build.artifactPath
```

- Inside one level, a non-empty list wins over the single path; blanks are dropped.
- A task can override the manifest per task: a non-empty task `artifacts` list wins, then a task `artifactPath`, and only then the manifest's own list, then its single path.
- A relative path is resolved against the remote project directory; a path starting with `/` or `~` is used as-is.
- After a successful build the daemon reconnects and checks each artifact on the target, then reports size and names anything missing in the job message.
- Artifacts are also what the daemon reports in a job: the job record carries `artifactPath` (the first artifact) and the full `artifacts` list.

## `cache.paths`

Build types declare the remote directories that should survive between builds, for example `.gradle/`, `ios/Pods/` or `.cgw/`.

These paths join the exclude set used by a push, so the local copy of a cache directory never overwrites the remote one. A push excludes, in order:

- the project's own excludes, which default to `.git`, `.DS_Store` and `Thumbs.db`;
- the fixed host-local artifact blacklist (things like `DerivedData`, `.cgw`, `build`, `.gradle`, `node_modules`, `.dart_tool`, `Pods`, `target` — these are never pushed);
- `cache.paths` from every manifest the project uses.

An exclude entry matches by name at any depth (`.git` also matches `sub/.git`), or by relative-path prefix when the entry contains `/` (`ios/Pods/` covers everything below it).

## Managing build types

The CLI covers the whole surface:

```bash
cgw manifests                      # list (same as `manifests list`)
cgw manifests status               # where the directory is, how many, which are invalid, which templates are missing
cgw manifests show <id>            # print the raw JSON
cgw manifests save <id> --file <path>   # or --file - to read stdin; an existing id needs --force
cgw manifests rm <id>
cgw manifests templates            # which shipped templates are missing
cgw manifests reset [id...]        # write back ONLY missing template files
cgw manifests refresh              # re-read the directory and report added / changed / removed
cgw manifests detect <host dir>
cgw manifests import <file.json> [--id <name>] [--force]
cgw manifests export <id> [--save <path>]   # without --save it prints to stdout
cgw manifests schema               # same as `cgw schema manifest`
```

Notes:

- `reset` only writes files that do not exist. It never touches a manifest you edited; to get a pristine template back, delete it first and then reset.
- `import` and `save` refuse to overwrite an existing id unless `--force` is given, so a script cannot clobber your build type silently.
- `refresh` diffs against the state seen earlier in this run (a baseline is taken at startup), so the first refresh does not report all four templates as new. Deleting the whole directory and refreshing recreates it and re-seeds the templates.
- The **WebUI build-type page** offers the same work: a toolbar with refresh, "restore missing templates" (only shown when something is missing) and new; a detect box with a run button; and per card, edit the raw JSON, export the file, and delete. Because the browser cannot read a file the user picks, "new" asks only for an id and writes a starter manifest — use `cgw manifests import` to bring in a file from disk.
- Manifest ids are restricted to letters, digits, `-` and `_`; the save API rejects anything else. Saving a manifest that fails validation returns a `400` with the full list of validation messages.

## JSON Schema

Every build type must satisfy a JSON Schema that is **generated from the Rust struct** — it cannot drift from the implementation:

```bash
cgw schema manifest
```

The same schema is available as the MCP resource `cgw://schema/manifest` and over HTTP as `GET /api/schema/manifest`. It is a draft-07 schema that states exactly what the code enforces: `displayName` and `build` are required, `build.steps` needs at least one item, `variables[].name` is non-empty, `default` is a nullable string, and `steps` is an `anyOf` of the two accepted step shapes.

## Worked examples

### The shipped `xcode` manifest

Taken verbatim from `src-tauri/templates/xcode.json`, with a named step:

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

### A minimal two-step manifest

Two unnamed steps, no variables, no cache:

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

Save it with `cgw manifests save make --file make.json`, and it is immediately selectable on a `build` task. Build types are used by tasks; see [Tasks](Tasks) for `manifest`, `variables`, `signingMode` and the per-task artifact override, and [CLI](CLI) for the full command reference.

← [Home](Home)
