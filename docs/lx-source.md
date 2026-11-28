# 洛雪音源怎么接（实操版）

这份文档里每条命令、每个接口参数都对着源码/文档核过，不是凭印象写的。

---

## 0. 先认清结构：洛雪音源是三层，不是一层

| 能力 | 归谁管 | 能不能拿来用 |
| --- | --- | --- |
| **搜索**（按歌名找到 songId） | 洛雪客户端内置的各平台 SDK `musicSdk/{kw,kg,tx,wy,mg}` | 不在脚本里、不在服务端接口里，只能移植源码或借第三方 API |
| **歌词 / 直链 / 详情** | `lx-music-api-server`（Python FastAPI 服务） | 可以，起服务后就是普通 HTTP 接口 |
| **自定义源脚本**（用户导入的 `*.js`） | 洛雪内置 JS 沙箱 | 只有 `inited` / `request(musicUrl \| lyric \| pic)`，**也没有搜索** |

自定义源脚本的 API 全集（来自官方《自定义源脚本编写说明》）：

- `lx.env` / `lx.version` / `lx.currentScriptInfo` / `lx.EVENT_NAMES`
- `lx.on(event, handler)`：脚本注册回调，**回调必须返回 Promise**
- `lx.send(event, data)`：脚本主动上报，初始化完成要 `send(lx.EVENT_NAMES.inited, { sources })`
- `lx.request(url, options, callback)`：`options` = `method / headers / body / form / formData / timeout`
- `lx.utils.buffer.from` / `bufToString`；`lx.utils.crypto.aesEncrypt(buffer, mode, key, iv)`（仅 `aes-128-cbc`、`aes-128-ecb`）/ `md5` / `randomBytes` / `rsaEncrypt`；`lx.utils.zlib.inflate/deflate`

**所以「配一个地址就接上洛雪搜索」这件事不存在。** 下面按你需要的能力给三条路线。

---

## 路线 A：自建 lx-music-api-server（15 分钟，拿 kw/kg/tx/wy 的歌词与直链）

### A1. 装依赖

```bash
brew install redis uv
brew services start redis        # 本机 redis 默认无密码
```

### A2. 拉代码、起服务

```bash
git clone https://github.com/MeoProject/lx-music-api-server.git ~/lx-music-api-server
cd ~/lx-music-api-server
uv sync
uv run main.py
```

- 默认监听 `127.0.0.1:8080`（`server.port`）
- 首次启动会生成 `./data/config.json`
- 默认配置里 `redis.password` 写的是 `"default"`，本机 Redis 没密码 → **启动前把这项改成 `""`**，否则连不上 Redis
- 需要 QQ / 酷狗的完整能力时，在 `data/config.json` 的 `modules.platform.tx.users`、`modules.platform.kg.users` 里填 cookie（`kw` / `wy` 不需要）

### A3. 三个接口（参数名是 `songId`，不是 `id`）

```bash
curl -s "http://127.0.0.1:8080/lyric?source=wy&songId=186016" | head -c 200
curl -s "http://127.0.0.1:8080/url?source=wy&songId=186016&quality=320k"
curl -s "http://127.0.0.1:8080/info?source=wy&songId=186016"
```

统一返回 `{"code":0,"msg":"...","data":{...}}`，`/lyric` 的歌词在 `data.lyric`。

**它没有 `/search`**，也不支持 `mg`，所以只能「已知 songId 时」补歌词和直链。

### A4. 接到项目里

```bash
FIREFLY_LX_API=http://127.0.0.1:8080 ./target/debug/FireflyMusic
```

对应代码：`src/services/lx.rs` 的 `lx_server_lyric(source, song_id)`、`song_url(source, song_id, quality)`。
现在的匹配链路**不依赖它**（咪咕搜索自带歌词），它是给播放器取直链、或将来接了 kw/kg/tx 搜索之后用的。

---

## 路线 B：要 QQ / 酷狗 的搜索（洛雪客户端自己也是这么干的）

洛雪能搜 QQ，是因为客户端里硬编码了 QQ 的搜索接口和签名（`musicSdk/tx`）。三条替代做法，实测结果不同：

### B1. 第三方聚合 API（只部分平台可用）

```bash
# 通的
curl -s "https://music-api.gdstudio.xyz/api.php?types=search&source=netease&name=%E6%99%B4%E5%A4%A9&count=20&pages=1"
curl -s "https://music-api.gdstudio.xyz/api.php?types=search&source=joox&name=%E6%99%B4%E5%A4%A9&count=20&pages=1"

# 实测 400：tencent / kugou / kuwo / migu / ytmusic（这类平台要它的会员 key）
```

字段：`id / name / artist[] / album / pic_id / lyric_id / source`，`id` 是平台原生 songId。
**结论：免费额度只够 netease + joox，撑不起 QQ/酷狗的缺口。**

### B2. 直接调平台公开搜索接口（推荐，洛雪内部也是这套）

| 平台 | 接口 | 实测 |
| --- | --- | --- |
| 酷狗 | `http://mobilecdn.kugou.com/api/v3/search/song?keyword=<kw>&page=1&pagesize=20` | 通，返回 `data.info[]`（`songname` / `singername` / `albumname` / `hash` / `album_id`）。**只能用 http**，https 会证书主机名不匹配 |
| 酷我 | `https://search.kuwo.cn/r.s?all=<kw>&ft=music&itemset=web_2013&client=kt&pn=0&rn=20&rformat=json&encoding=utf8` | 通，但返回的是单引号伪 JSON，要先做一次宽松替换再解析 |
| 咪咕 | 已在用（`app.c.nf.migu.cn` 的 `search_all.do`） | 最准，还直接给 LRC |
| QQ / 网易云 | 需要 `sign` 签名（QQ）或已降级只返回翻唱（网易云 web 接口） | 不适合直接抄 |

酷狗歌词要多一步：`https://krcs.kugou.com/search?ver=1&man=yes&client=mobi&keyword=&duration=&hash=<hash>` 拿到 `id` + `accesskey`，
再 `https://lyrics.kugou.com/download?ver=1&client=pc&id=<id>&accesskey=<key>&fmt=lrc&charset=utf8`，返回 base64 的 LRC。

### B3. 移植洛雪 SDK（半天到一天）

抄 `lx-music-desktop/src/renderer/utils/musicSdk/` 下 `kw`、`kg`、`tx` 三个目录，
好处是不依赖第三方，坏处是各家一改接口就得跟。

---

## 路线 C：真跑自定义源脚本（Node 适配器，1~2 小时）

脚本本身不搜索，只对给定 songId 返回 `musicUrl` / `lyric` / `pic`，所以适配器要做的是：
在 Node 里造一个 `globalThis.lx`，再暴露一个 HTTP 接口让 Rust 调。

骨架（未逐脚本调通，实际脚本常用到 `lx.utils.crypto`，需要按报错补方法）：

```js
// scripts/lx-adapter.mjs —— 用法: node scripts/lx-adapter.mjs sixyin.js 8787
import { readFileSync } from 'node:fs'
import { createServer } from 'node:http'
import crypto from 'node:crypto'
import zlib from 'node:zlib'
import vm from 'node:vm'

const [scriptPath, portArg] = process.argv.slice(2)
const port = Number(portArg || 8787)
const source = readFileSync(scriptPath, 'utf8')

const EVENT_NAMES = { inited: 'inited', request: 'request', updateAlert: 'updateAlert' }
const handlers = new Map()
let inited = null

const request = (url, options = {}, callback) => {
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), options.timeout || 15000)
  const body = options.form ? new URLSearchParams(options.form).toString() : options.body
  fetch(url, { method: options.method || 'GET', headers: options.headers, body, signal: controller.signal })
    .then(async (resp) => callback(null, { statusCode: resp.status, headers: {}, body: await resp.text() }))
    .catch((err) => callback(err))
    .finally(() => clearTimeout(timer))
  return () => controller.abort()
}

globalThis.lx = {
  version: 2,
  env: 'desktop',
  currentScriptInfo: { name: 'adapter', rawScript: source },
  EVENT_NAMES,
  on: (name, handler) => handlers.set(name, handler),
  send: (name, data) => { if (name === EVENT_NAMES.inited) inited = data },
  request,
  utils: {
    buffer: { from: (d, f) => Buffer.from(d, f), bufToString: (b, f) => b.toString(f) },
    crypto: {
      aesEncrypt: (buffer, mode, key, iv) => {
        const algo = mode === 'aes-128-ecb' ? 'aes-128-ecb' : 'aes-128-cbc'
        const cipher = crypto.createCipheriv(algo, key, iv || null)
        return Buffer.concat([cipher.update(buffer), cipher.final()])
      },
      md5: (str) => crypto.createHash('md5').update(str).digest('hex'),
      randomBytes: (size) => crypto.randomBytes(size).toString('hex'),
      rsaEncrypt: (buffer, key) =>
        crypto.publicEncrypt({ key, padding: crypto.constants.RSA_PKCS1_PADDING }, buffer),
    },
    zlib: { inflate: async (b) => zlib.inflateSync(b), deflate: async (b) => zlib.deflateSync(b) },
  },
}

vm.runInThisContext(source, { filename: scriptPath })
if (!inited) throw new Error('脚本没有发送 inited，初始化失败')
console.log('已加载源:', Object.keys(inited.sources || {}).join(', '))

createServer(async (req, res) => {
  if (req.url === '/health') return res.end(JSON.stringify({ ok: !!inited, sources: inited.sources }))
  const chunks = []
  for await (const chunk of req) chunks.push(chunk)
  const { source: platform, action, info } = JSON.parse(Buffer.concat(chunks).toString() || '{}')
  const handler = handlers.get(EVENT_NAMES.request)
  try {
    const data = await handler({ source: platform, action, info })
    res.end(JSON.stringify({ code: 0, data }))
  } catch (error) {
    res.end(JSON.stringify({ code: 1, message: String(error && error.message) }))
  }
}).listen(port, '127.0.0.1', () => console.log(`adapter on http://127.0.0.1:${port}`))
```

跑法：

```bash
node scripts/lx-adapter.mjs .scratch/sixyin.js 8787
curl -s localhost:8787/health
curl -s -X POST localhost:8787/invoke -d '{"source":"tx","action":"musicUrl","info":{"type":"320k","musicInfo":{"songmid":"0039MnYb0qxYhV"}}}'
```

注意：`info.musicInfo` 的字段名是各平台自己的（tx 用 `songmid`、kw 用 `musicId`、wy 用 `musicId`），
得先有 songId —— 又回到路线 B 的搜索问题。

---

## 选哪条

| 你想要的 | 走哪条 | 成本 |
| --- | --- | --- |
| 只是让「匹配元数据」更全（元数据 + 歌词） | 现在的咪咕直连已经够，不用动 | 0 |
| 播放器要能真的播歌（直链） | 路线 A，把 `FIREFLY_LX_API` 指向自建服务 | 15 分钟 |
| 要酷狗 / 酷我的曲库 | 路线 B2，把这两家的搜索接进 `lx.rs` | 1~2 小时 |
| 要 QQ 音乐或完全离线自持 | 路线 B3 移植 SDK + 路线 A | 1~2 天 |
| 想在程序里直接跑用户导入的 `*.js` | Rust 里嵌 JS 引擎（boa / quickjs）+ 自己实现 `lx.utils.crypto` 全套 | 数天，不建议现在做 |

---

## 备注

- `lx-music-api-server` 的协议：搜索未实现；`/url` 需要 `quality`（`128k` / `320k` / `flac` / `flac24bit`）；`kg` 的 `songId` 会被转小写。
- 该服务带 `AuthMiddleware`，默认只做了 UA 黑名单检查，不校验 token；如果要暴露到公网，务必自己在反向代理上加鉴权。
- 本项目里相关代码：`src/services/lx.rs`（`match_track` / `lx_server_lyric` / `song_url` / `write_lyric`）。
