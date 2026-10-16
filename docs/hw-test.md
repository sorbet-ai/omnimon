# Hardware check: NVLink, MIG, GPM

NVLink, MIG and GPM were built against NVIDIA's documented API (constants and
struct layouts checked against `nvidia-ml-py`) and the demo backend, but have
not met real hardware yet. One session on a rented machine covers all three.

## The machine

- 2 or more **SXM** A100 or H100 (PCIe cards usually have no NVLink). H100
  also exercises GPM.
- A full VM with root (enabling MIG needs it; most container rentals refuse).
- Driver r535 or newer, and `python3` with PyTorch for the load below.

## Steps (about 30 minutes)

Copy `dist/omnimon-x86_64-linux` over, then:

```sh
# 1. What was detected. Expect "GPM true, NVLink true" per GPU.
OMNIMON_LOG=debug ./omnimon --diagnose 2>&1 | grep -E "GPM|NVLink|MIG"
./omnimon --json > idle.json

# 2. Reference values to compare against.
nvidia-smi nvlink -s          # link speeds / state
nvidia-smi nvlink -e          # error counters
nvidia-smi topo -m            # who is linked to whom

# 3. NVLink traffic: copy between GPU 0 and 1 in a loop, watch the
#    Details tab (NVLink block) and the NVL rates on the cards.
python3 - <<'EOF' &
import torch, time
a = torch.empty(1 << 28, device="cuda:0")
b = torch.empty(1 << 28, device="cuda:1")
t = time.time()
while time.time() - t < 120:
    b.copy_(a); a.copy_(b)
torch.cuda.synchronize()
EOF
./omnimon          # Details tab on GPU 0; compare with: nvidia-smi nvlink -gt d

# 4. GPM / tensor cores (H100): a bf16 matmul loop should show high
#    "Tensor cores" (fp16/bf16) and memory bandwidth on the card.
python3 -c "
import torch, time
x = torch.randn(8192, 8192, device='cuda:0', dtype=torch.bfloat16)
t = time.time()
while time.time() - t < 60: x @ x
" &
./omnimon

# 5. MIG on GPU 1 (needs root; may need a GPU reset or reboot).
sudo nvidia-smi -i 1 -mig 1
sudo nvidia-smi mig -i 1 -lgip            # list profiles and their ids
sudo nvidia-smi mig -i 1 -cgi <id>,<id> -C # e.g. a 3g and a 2g profile
nvidia-smi -L                              # MIG UUIDs
CUDA_VISIBLE_DEVICES=MIG-<uuid> python3 -c "
import torch, time
x = torch.randn(4096, 4096, device='cuda')
t = time.time()
while time.time() - t < 60: x @ x
" &
./omnimon          # GPU 1 card shows MIG 2x; Details lists the partitions;
                   # the process shows as 1/<instance> in the table
./omnimon --json > mig.json

# 6. Clean up.
sudo nvidia-smi mig -i 1 -dci && sudo nvidia-smi mig -i 1 -dgi
sudo nvidia-smi -i 1 -mig 0
```

Bring back `idle.json`, `mig.json`, the `--diagnose` output, and anything that
looked wrong next to the matching `nvidia-smi` numbers.
