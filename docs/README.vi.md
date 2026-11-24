# kaggle-shell-mcp (`kgsh`) ⚡ (Tài liệu Tiếng Việt)

**Biến GPU Kaggle 12h miễn phí thành hệ thống Multi-Slot Live Terminal & Offline Competition Runner cho AI Agent qua MCP.**

---

## Điểm nổi bật của `kaggle-shell-mcp` (v0.2)

1. **Cài đặt 1 dòng qua PyPI / `uvx` — Không cần VPN hay tài khoản trung gian**: Chỉ cần `KAGGLE_USERNAME` và `KAGGLE_KEY` (`KGAT_...`, key hex cũ, hoặc tự nhận sẵn `~/.kaggle/kaggle.json`). Tự động mở đường truyền Cloudflare Quick Tunnel và trao đổi địa chỉ qua Private Dataset riêng cho từng slot.
2. **Chạy song song 2 máy GPU cùng lúc (`4 × Tesla T4 = 60 GB VRAM`)**:
   - Hỗ trợ đa `slot` (`kgsh up -s 1,2`) khởi chạy đồng thời 2 máy ảo GPU độc lập không đụng độ trạng thái.
   - Hỗ trợ ghim card lẻ (`gpu_ids="0"` hoặc `"1"`) trong `job_spawn` để chạy **4 thí nghiệm GPU độc lập cùng lúc** trên 2 máy.
3. **Mở khóa toàn bộ Phần cứng, Competition & Kaggle Models**:
   - Hỗ trợ `t4` (`NvidiaTeslaT4`), `l4`, `l4x4` (`96 GB`), `rtx6000` (`NvidiaRtxPro6000` — `96 GB`), `h100`, `tpuv5e8` / `tpuv6e8` (tự động hạ trần safe drain xuống `8h40m` cho TPU 9h), và `cpu`.
   - Gắn trực tiếp dữ liệu cuộc thi (`competition_sources`) và model lớn từ Kaggle Hub (`model_sources`) vào `/kaggle/input` trong 0 giây.
4. **Chế độ Offline Competition + Đọc Live Log không cần Internet**:
   - Với các giải ép `enable_internet=False` trên `L4x4` / `RTX Pro 6000`, bộ 3 công cụ `kaggle_wheelhouse` + `kaggle_batch_run` + `kaggle_batch_logs` tự đóng gói wheel Linux + code và đọc live log thời gian thực qua luồng **Host SSE** của Kaggle mà không cần mở trình duyệt.
5. **Giữ 100% môi trường CUDA & Bảo vệ Snapshot `/kaggle/working`**:
   - Chạy trực tiếp Python PTY/RPC daemon trong tiến trình Kernel, tự lọc ký tự `\r` của thanh `tqdm`.
   - Tự gửi `SIGTERM` lúc `11h35m` để lưu checkpoint và thoát sạch `exit(0)` lúc `11h40m` trước mốc kill 12h của Kaggle (tuỳ chọn bật thêm tự tắt khi rỗi qua `--idle 30`).

---

## Cài đặt & Kết nối MCP (30 giây)

### Cách 1: Chạy trực tiếp qua `uvx` (Không cần tự tạo `.venv`)

Dán thẳng vào `mcp_config.json` của **Claude Desktop / Cursor / Windsurf / Antigravity**:

```json
{
  "mcpServers": {
    "kgsh": {
      "command": "uvx",
      "args": ["--from", "kaggle-shell-mcp", "kgsh"],
      "env": {
        "KAGGLE_USERNAME": "ten_tai_khoan_kaggle",
        "KAGGLE_KEY": "KGAT_token_cua_ban"
      }
    }
  }
}
```

Hoặc gắn vào **Claude Code CLI** bằng 1 lệnh:
```bash
claude mcp add kgsh -e KAGGLE_USERNAME=ten_ban -e KAGGLE_KEY=KGAT_token -- uvx --from kaggle-shell-mcp kgsh
```

### Cách 2: Cài lệnh `kgsh` toàn cục qua `pip` / `uv`

```powershell
pip install kaggle-shell-mcp
# hoặc cài độc lập qua uv:
uv tool install kaggle-shell-mcp
```

Công cụ tự động đọc thông tin xác thực theo thứ tự:
1. Biến môi trường `KAGGLE_USERNAME` + `KAGGLE_KEY`,
2. File `.env` ở thư mục hiện tại hoặc `~/.kaggle_mcp/.env`,
3. File chuẩn `~/.kaggle/kaggle.json` nếu máy đã đăng nhập Kaggle trước đó.

---

## Danh sách 12 MCP Tools cho AI Agent

| MCP Tool | Chức năng |
| :--- | :--- |
| `kaggle_up` | Khởi chạy 1 hoặc 2 máy GPU chạy nền song song (`slot="1"`, `"2"`, hoặc `"1,2"`, chọn `accelerator`, `competition`, `mount_models`) |
| `kaggle_status` | Xem trạng thái kernel, % GPU/VRAM, RAM, ổ đĩa trên tất cả các slot (`slot="all"`) và tự dọn phiên cũ đã hủy |
| `kaggle_down` | Tắt sạch 1 slot hoặc toàn bộ (`slot="all"`) ngay lập tức để tiết kiệm giờ GPU; tuỳ chọn kéo `/kaggle/working` |
| `term_exec` | Chạy lệnh bash tương tác trong PTY giữ trạng thái (`cd`, biến môi trường, `venv`) theo từng `slot` |
| `job_spawn` | Chạy ngầm tác vụ train dài hạn (rớt mạng không chết), hỗ trợ ghim card lẻ `gpu_ids="0"` hoặc `"1"` |
| `job_list` | Liệt kê danh sách tiến trình chạy ngầm trên `slot` |
| `job_logs` | Đọc log tiến trình ngầm (đã tự gọt sạch thanh tiến trình `tqdm` `\r`) |
| `job_kill` | Gửi `SIGTERM` / `SIGKILL` dừng tiến trình ngầm và giải phóng VRAM |
| `fs_sync` | Đồng bộ nhanh thư mục code (`push`) hoặc kéo checkpoint/log (`pull`) theo `slot` |
| `kaggle_wheelhouse` | Tải bộ `.whl` chuẩn Linux `manylinux_x86_64` đẩy lên Private Dataset để cài khi tắt mạng |
| `kaggle_batch_run` | Đóng gói code chạy giải đấu khóa mạng (`enable_internet=False`) trên `L4x4` / `RTX Pro 6000` / `TPU` |
| `kaggle_batch_logs` | Đọc live log thời gian thực từ tầng Host SSE của Kaggle kể cả khi container tắt sạch internet |

---

## Ví dụ sử dụng qua CLI (`kgsh`)

### 1. Bật song song 2 máy GPU & chia 4 job trên 4 card T4
```powershell
# Bật cùng lúc 2 máy T4x2 (~60–95s)
kgsh up -s 1,2

# Đẩy code lên cả 2 máy
kgsh push .\my_app /kaggle/working -s 1
kgsh push .\my_app /kaggle/working -s 2

# Chạy 4 cấu hình thí nghiệm độc lập trên 4 card GPU
kgsh spawn -s 1 --gpus 0 "python my_app/train.py --seed 1" --name s1g0
kgsh spawn -s 1 --gpus 1 "python my_app/train.py --seed 2" --name s1g1
kgsh spawn -s 2 --gpus 0 "python my_app/train.py --seed 3" --name s2g0
kgsh spawn -s 2 --gpus 1 "python my_app/train.py --seed 4" --name s2g1

# Kiểm tra tất cả slot & tắt toàn bộ khi xong việc
kgsh status
kgsh down -s all
```

### 2. Đẩy chạy Competition khóa mạng (`L4x4` / `RTX Pro 6000`)
```powershell
# Tải sẵn thư viện Linux đẩy lên Private Dataset (chỉ cần làm 1 lần)
kgsh wheels peft bitsandbytes --slug lora --py 312

# Đẩy job offline trên phần cứng cuộc thi
kgsh batch-run "torchrun --nproc_per_node=4 train.py" --path .\solution -a rtx6000 --comp ten-cuoc-thi --wheels ten_ban/kgsh-wheels-lora

# Đọc live log trực tiếp không cần mở trình duyệt
kgsh batch-logs -s batch-1 --tail 80
```
