# kaggle-shell-mcp (`kgsh`) ⚡ (Tài liệu Tiếng Việt)

**Biến phiên GPU Kaggle 12h miễn phí (Dual NVIDIA T4 × 2 = 30 GB VRAM) thành máy trạm điều khiển trực tiếp qua Terminal & MCP cho AI Agent.**

---

## Điểm khác biệt của `kaggle-shell-mcp`

1. **Không cần cài VPN hay tài khoản trung gian**: Chỉ cần duy nhất `KAGGLE_USERNAME` và `KAGGLE_KEY` (`KGAT_...` hoặc key hex cũ) trong `.env`. Tự động mở đường truyền qua Cloudflare Quick Tunnel và đồng bộ địa chỉ qua Private Dataset `<username>/kaggle-mcp-state` (vẫn hỗ trợ `TS_AUTHKEY` nếu muốn chạy qua mạng riêng Tailscale).
2. **Giữ nguyên 100% môi trường CUDA/PyTorch**: Không cài `openssh-server` qua `apt`, chạy trực tiếp Python PTY/RPC daemon bên trong tiến trình Kaggle Kernel.
3. **Tách biệt lệnh ngắn & tiến trình train dài**:
   - `term_exec`: Bash PTY giữ trạng thái (`cd`, `export`, `conda`/`venv`) cho các thao tác kiểm tra nhanh.
   - `job_spawn` / `job_logs`: Chạy ngầm độc lập với kết nối mạng, tự gọt ký tự `\r` của thanh `tqdm` để không làm tràn context của Agent.
4. **Bảo vệ Quota 30h/tuần & Chống mất dữ liệu ở mốc 12h**:
   - Tự gửi `SIGTERM` lúc `11h35m` và thoát sạch `exit(0)` lúc `11h40m` để Kaggle lưu trọn vẹn 20 GB `/kaggle/working` thay vì bị kill lỗi timeout ở `12h00m`.
   - Tuỳ chọn bật tự tắt khi rỗi (`idle_timeout_min` / `--idle 30`, mặc định `0` = tắt để giữ phiên sẵn sàng tối đa 11h40m).

---

## Cài đặt nhanh (Python 3.11+)

```powershell
git clone https://github.com/NguyenQuocDat06-AI/kaggle-shell-mcp.git
cd kaggle-shell-mcp
python -m venv .venv
.\.venv\Scripts\pip install -e .
```

Tạo file `.env`:
```ini
KAGGLE_USERNAME=ten_tai_khoan_kaggle
KAGGLE_KEY=KGAT_token_cua_ban
```

## Cấu hình MCP (`mcp_config.json`)

```json
{
  "mcpServers": {
    "kgsh": {
      "command": "C:/duong/dan/toi/kaggle-shell-mcp/.venv/Scripts/kgsh.exe",
      "args": []
    }
  }
}
```

## Sử dụng nhanh qua dòng lệnh (`kgsh`)

```powershell
# Kiểm tra trạng thái (không tốn quota)
kgsh status

# Bật máy Dual T4 (~50 giây; thêm --idle 30 nếu muốn tự tắt khi rỗi)
kgsh up

# Chạy lệnh trực tiếp trên máy Kaggle
kgsh exec "nvidia-smi"

# Đẩy thư mục code từ máy lên /kaggle/working/my_app
kgsh push .\my_app /kaggle/working

# Chạy train ngầm trên cả 2 card T4
kgsh spawn "torchrun --nproc_per_node=2 my_app/train.py" --name run1
kgsh logs <job_id> --tail 50

# Kéo kết quả về & tắt máy ngay để tiết kiệm giờ GPU
kgsh pull /kaggle/working/outputs .\outputs
kgsh down
```
