# Windows 与 Android 互通验收

从仓库根目录运行。使用已联网到 `AndroidWifi` 的专用 Android Studio 模拟器（API 29 以上，原生英文或中文系统）。应用应没有已加入的群组。测试会切换模拟器的 Wi-Fi 和移动网络，并在 Downloads 中生成一个约 1 MiB 的往返校验文件。可用 `-read-only -no-snapshot-load -no-snapshot-save -no-window` 启动临时实例。手机上的普通使用方法见[项目说明](../../README.md)。

先复用本机 SDK、NDK、Gradle 与 Cargo 缓存构建并安装应用与测试包。将下面的序列号换成专用模拟器的实际值。

```powershell
$serial = 'emulator-5570'
$env:CARGO_NET_OFFLINE = 'true'
cargo build --locked --offline -p core --example android_test_relay
.\android-app\gradlew.bat -p android-app --offline assembleDebug assembleDebugAndroidTest
adb -s $serial install -r android-app/app/build/outputs/apk/debug/app-debug.apk
adb -s $serial install -r android-app/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk
```

在第一个 PowerShell 窗口启动 Windows 测试对端。它创建 Alpha、Beta 两个群组。Alpha 包含两台逻辑中继。模拟器的 NAT 通过 UDP 端口重定向接收真实协议公告。保持窗口运行，并记下打印的目录。

```powershell
$run = Join-Path (Resolve-Path target) ("android-interop-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $run | Out-Null
Copy-Item target/debug/examples/android_test_relay.exe $run
adb -s $serial emu redir add udp:39724:37020
Write-Output $run
& "$run/android_test_relay.exe" $run 39724
```

在第二个 PowerShell 窗口使用相同的序列号与目录，按顺序运行两项测试。两次输出都应包含 `OK (1 test)`。第二项复用第一项建立的群组来验证进程重启恢复。

```powershell
$serial = 'emulator-5570'
$run = Read-Host '输入第一个窗口打印的目录'
$peers = Get-Content (Join-Path $run relay.json) -Raw | ConvertFrom-Json
$runner = 'org.lanmesh.android.test/androidx.test.runner.AndroidJUnitRunner'
$testArgs = @('-e', 'isolatedEmulator', 'yes', '-e', 'groupA', $peers.groupA,
    '-e', 'groupB', $peers.groupB, '-e', 'peerA', $peers.peerA, '-e', 'portA', $peers.portA)

adb -s $serial shell am instrument -w -r @testArgs -e class 'org.lanmesh.android.InteropTest#windowsRelayAndAndroidClient' $runner
adb -s $serial shell am force-stop org.lanmesh.android
adb -s $serial shell am instrument -w -r @testArgs -e class 'org.lanmesh.android.InteropTest#restoresIdentitiesAndStopsAfterLastGroup' $runner
adb -s $serial emu redir del udp:39724
```

测试覆盖发现和手动加入、两跳群聊和私聊、前后台收信、两个群组重新接入 Wi-Fi 以及退出群组。收发文件各留一个分片触发补传，再用系统文件选择器导出，并校验导出内容的 SHA256。结束后在第一个窗口按 Ctrl+C 停止中继，并关闭专用模拟器。
