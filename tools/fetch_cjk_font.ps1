# Downloads the fallback CJK fonts (Noto Sans SC and KR, SIL OFL 1.1, ~28 MB) into crates/io/assets/fonts/cjk/.
# They are not stored in git because of their size; without them the CJK test is skipped and CJK in PDF is drawn as .notdef.
$ErrorActionPreference = 'Stop'
$rev = 'a85815a42757630ce188fdad368c2dfc444d4773'
$fonts = @(
  @{ n = 'NotoSansSC'; sha = 'a3041811a78c361b1de50f953c805e0244951c21c5bd412f7232ef0d899af0da' },
  @{ n = 'NotoSansKR'; sha = '194018e6b2b293a7964f037b25c0249ce1418bc9ab3c971060a03aa57861e252' }
)
$dir = Join-Path $PSScriptRoot '..\crates\io\assets\fonts\cjk'
New-Item -ItemType Directory -Force $dir | Out-Null
foreach ($f in $fonts) {
  $dst = Join-Path $dir "$($f.n).ttf"
  $url = "https://github.com/google/fonts/raw/$rev/ofl/$($f.n.ToLower())/$($f.n)%5Bwght%5D.ttf"
  Invoke-WebRequest $url -OutFile $dst
  $got = (Get-FileHash $dst -Algorithm SHA256).Hash.ToLower()
  if ($got -ne $f.sha) { Remove-Item $dst; throw "sha256 mismatch $($f.n): $got" }
  Write-Host "OK $dst"
}
