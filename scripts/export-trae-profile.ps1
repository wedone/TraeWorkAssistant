<#
.SYNOPSIS
  导出本机 Trae CN 客户端环境档案（实机环境），供网关「全局 API 管理 → 实机环境」导入。

.DESCRIPTION
  读取真机的客户端版本、设备指纹与 icube 设备凭证，输出单个 JSON：
  - identity  : manifest.json / product.json 的真实版本身份
  - device    : telemetry.machineId、device_id、系统型号/厂商/CPU/系统版本
  - credential: storage.json 中 iCubeAuthInfo://icube-dc:<deviceId> 的 tc 信封密文
                （私钥明文不在导出文件里，后端导入时解密）

  必须与 Python 版 scripts/export_trae_profile.py 输出同构。

.PARAMETER OutFile
  输出路径，默认 ./trae-client-profile.json

.PARAMETER AppDir
  Trae CN 安装目录，默认扫描 $env:LOCALAPPDATA\Programs\Trae CN

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\export-trae-profile.ps1
#>
param(
  [string]$OutFile = (Join-Path (Get-Location) 'trae-client-profile.json'),
  [string]$AppDir = ''
)

$ErrorActionPreference = 'Stop'

function Read-JsonFile([string]$path) {
  if (-not (Test-Path -LiteralPath $path)) { return $null }
  try { return Get-Content -LiteralPath $path -Raw -Encoding UTF8 | ConvertFrom-Json }
  catch { return $null }
}

# ---- 定位安装目录 ----
$candidates = @()
if ($AppDir) { $candidates += $AppDir }
else {
  $candidates += (Join-Path $env:LOCALAPPDATA 'Programs\Trae CN')
  $candidates += (Join-Path $env:LOCALAPPDATA 'Programs\TRAE SOLO CN')
}

$installed = $null
foreach ($c in $candidates) {
  if (Test-Path -LiteralPath (Join-Path $c 'manifest.json')) { $installed = $c; break }
}
if (-not $installed) {
  throw "未找到 Trae 客户端安装目录（已尝试: $($candidates -join ', ')）。请用 -AppDir 指定。"
}

$manifest = Read-JsonFile (Join-Path $installed 'manifest.json')
if (-not $manifest) { throw "manifest.json 读取失败: $installed" }
$product = Read-JsonFile (Join-Path $installed 'resources\app\product.json')

$appVersion = [string]$manifest.appVersion
$buildVersion = [string]$manifest.buildVersion
$packageType = [string]$manifest.packageType
$appId = [string]$manifest.appId
$versionCode = [string]$manifest.ahaNet.ttnet_params.version_code
if (-not $versionCode -and $product) { $versionCode = [string]$product.traeVersionCode }

# ---- 用户数据（storage.json）----
$appName = Split-Path -Leaf $installed
$storagePath = Join-Path $env:APPDATA (Join-Path $appName 'User\globalStorage\storage.json')
$storage = Read-JsonFile $storagePath
if (-not $storage) {
  throw "storage.json 读取失败: $storagePath（客户端至少启动登录过一次才会生成）"
}

$props = $storage.PSObject.Properties
$machineId = [string]($props | Where-Object { $_.Name -eq 'telemetry.machineId' } | Select-Object -First 1).Value
$devDeviceId = [string]($props | Where-Object { $_.Name -eq 'telemetry.devDeviceId' } | Select-Object -First 1).Value

$credKey = $props | Where-Object { $_.Name -like 'iCubeAuthInfo://icube-dc:*' } | Select-Object -First 1
if (-not $credKey) {
  Write-Warning '未找到 icube 设备凭证（iCubeAuthInfo://icube-dc:*）：导入后将无私钥，DeviceInfo/DeviceProof 无法与真机同源'
}
$deviceId = ''
$tcEnvelope = ''
$storageKey = ''
if ($credKey) {
  $storageKey = [string]$credKey.Name
  $tcEnvelope = [string]$credKey.Value
  $deviceId = $storageKey.Substring($storageKey.LastIndexOf(':') + 1)
}

# ---- 系统指纹（注册表口径，与客户端 systemInformation 一致）----
$bios = Get-ItemProperty 'HKLM:\HARDWARE\DESCRIPTION\System\BIOS' -ErrorAction SilentlyContinue
$cpuKey = Get-ItemProperty 'HKLM:\HARDWARE\DESCRIPTION\System\CentralProcessor\0' -ErrorAction SilentlyContinue
$win = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion' -ErrorAction SilentlyContinue

$deviceModel = if ($bios) { [string]$bios.SystemProductName } else { '' }
$deviceManufacturer = if ($bios) { [string]$bios.SystemManufacturer } else { '' }
$cpuName = if ($cpuKey) { [string]$cpuKey.ProcessorNameString } else { '' }
# 客户端 cpuManufacturer 取 CPU 型号解析出的厂商名
$cpu = if ($cpuName -match 'Intel') { 'Intel' } elseif ($cpuName -match 'AMD') { 'AMD' } else { $cpuName }
$osVersion = if ($win) { [string]$win.ProductName } else { '' }

$out = [ordered]@{
  schema      = 1
  exported_at = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
  source_app  = $appName
  identity    = [ordered]@{
    app_version   = $appVersion
    build_version = $buildVersion
    version_code  = $versionCode
    package_type  = $packageType
    app_id        = $appId
  }
  device      = [ordered]@{
    device_id           = $deviceId
    machine_id          = $machineId
    dev_device_id       = $devDeviceId
    device_model        = $deviceModel
    device_manufacturer = $deviceManufacturer
    cpu                 = $cpu
    os_name             = 'windows'
    os_version          = $osVersion
  }
  credential  = [ordered]@{
    storage_key = $storageKey
    tc_envelope = $tcEnvelope
  }
}

$json = $out | ConvertTo-Json -Depth 8
# 必须无 BOM：后端 serde_json 不跳过 BOM，带 BOM 会解析失败
[System.IO.File]::WriteAllText($OutFile, $json, (New-Object System.Text.UTF8Encoding($false)))

Write-Host "已导出: $OutFile"
Write-Host ("  客户端: {0} ({1}) / code {2}" -f $appVersion, $buildVersion, $versionCode)
Write-Host ("  设备: device_id={0} machine_id={1}" -f $(if ($deviceId) { $deviceId } else { '-' }), $(if ($machineId) { '已读取' } else { '-' }))
Write-Host ("  凭证: {0}" -f $(if ($tcEnvelope) { '已包含（密文）' } else { '缺失' }))
Write-Host '注意: 文件含设备凭证密文，请勿外传；导入网关后建议删除本地副本。'
