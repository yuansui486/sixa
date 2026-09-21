# 私匣模型发布目录

公开下载对象统一放在 Bucket `tct12` 的以下前缀：

```text
12box/
└── sixa/
    └── models/
        └── desktop-models-v1.0.0/
            └── desktop/
                ├── catalog.json
                ├── raner-v1.zip
                ├── ppocrv4-mobile-v1.zip
                └── ppocrv4-accurate-v1.zip
```

公网基础地址：

```text
https://tct12.oss-cn-beijing.aliyuncs.com/12box/sixa/models/desktop-models-v1.0.0/desktop/
```

文件清单：

| 文件 | 用途 | 字节数 | SHA-256 |
| --- | --- | ---: | --- |
| `catalog.json` | 应用固定的双源下载目录 | 1,646 | `94ee999d45dae02e38d54b509ece4788d082f6ea58e2930403f67c24f7a3f21d` |
| `raner-v1.zip` | 中文实体识别 RaNER | 383,443,560 | `4386188a3453e20f703feb009f3c731a1adea654b87747bfd292cca05ce3e6b1` |
| `ppocrv4-mobile-v1.zip` | PP-OCRv4 轻量 OCR | 20,617,575 | `b260c430ed85d3ebe0bbf37705cdbcf33a11be41be691ded77df470b4e83a832` |
| `ppocrv4-accurate-v1.zip` | PP-OCRv4 高精度 OCR | 185,636,512 | `6e9ded592fa160877168b5d1c51e802210473f645d2800c5c150548f922cde07` |

模型 ZIP 使用 `Cache-Control: public,max-age=31536000,immutable`。`catalog.json` 使用短缓存 `public,max-age=300`。四个对象允许匿名读取，不开放匿名上传、删除或目录列举。

发布新模型时新建版本目录，例如 `desktop-models-v1.1.0/desktop/`，不要覆盖旧版本 ZIP。先上传三个 ZIP，再上传引用它们的新 `catalog.json`，验证匿名 Range 下载和完整 SHA-256 后，最后更新应用内置目录与版本号。ModelScope 保留相同内容作为备用源。
