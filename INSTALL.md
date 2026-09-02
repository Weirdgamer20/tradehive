# Installation Guide for TradingHive v0.9.0

Complete installation instructions for Windows, Linux, and macOS.

## Table of Contents

1. [Windows Installation](#windows-installation)
2. [Linux Installation](#linux-installation)
3. [macOS Installation](#macos-installation)
4. [Post-Installation](#post-installation)
5. [Verification](#verification)

---

## Windows Installation

### Step 1: Install Rust

1. Download the installer from [rustup.rs](https://rustup.rs/)
2. Run `rustup-init.exe`
3. Choose option 1 (default installation)
4. Restart your terminal or computer

Verify installation:
```powershell
rustc --version
cargo --version
```

### Step 2: Install Git

1. Download from [git-scm.com](https://git-scm.com/download/win)
2. Run the installer with default settings
3. Restart your terminal

### Step 3: Clone TradingHive

```powershell
git clone https://github.com/yourusername/TradingHive-Rust-V17.git
cd TradingHive-Rust-V17
cd TradingHive-Rust-V17
```

### Step 4: Set Up Environment

```powershell
# Copy example environment file
Copy-Item .env.example .env

# Edit .env with your credentials (use Notepad or your editor)
notepad .env
```

Add your Alpaca credentials:
```env
APCA_API_KEY_ID=your_paper_key
APCA_API_SECRET_KEY=your_paper_secret
TRADING_HIVE_LIVE_CONFIRM=NO
```

### Step 5: Build the Project

```powershell
# Build in release mode (optimized)
cargo build --release

# This takes 2-5 minutes on first build
```

### Step 6: Verify Installation

```powershell
# Run tests
cargo test --all

# Should see: test result: ok. 11 passed
```

### Quick Run

```powershell
# Paper trading (safe)
cargo run --release -- session

# Or use the built binary directly
.\target\release\trading-hive.exe session
```

---

## Linux Installation

### Step 1: Install Rust

```bash
# Download and run rustup installer
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Follow prompts (press Enter for default)
# Source the environment
source $HOME/.cargo/env

# Verify installation
rustc --version
cargo --version
```

### Step 2: Install Dependencies

**Ubuntu/Debian:**
```bash
sudo apt-get update
sudo apt-get install -y build-essential git
```

**Fedora/RHEL:**
```bash
sudo dnf install -y gcc g++ git
```

**Arch:**
```bash
sudo pacman -S base-devel git
```

### Step 3: Clone TradingHive

```bash
git clone https://github.com/yourusername/TradingHive-Rust-V17.git
cd TradingHive-Rust-V17/TradingHive-Rust-V17
```

### Step 4: Set Up Environment

```bash
# Copy example environment file
cp .env.example .env

# Edit with your preferred editor
nano .env  # or: vim .env, code .env
```

Add your Alpaca credentials:
```env
APCA_API_KEY_ID=your_paper_key
APCA_API_SECRET_KEY=your_paper_secret
TRADING_HIVE_LIVE_CONFIRM=NO
```

### Step 5: Build the Project

```bash
# Build in release mode (optimized)
cargo build --release

# This takes 2-5 minutes on first build
# Uses about 1-2 GB disk space
```

### Step 6: Verify Installation

```bash
# Run tests
cargo test --all

# Should see: test result: ok. 11 passed
```

### Quick Run

```bash
# Paper trading (safe)
cargo run --release -- session

# Or use the built binary directly
./target/release/trading-hive session
```

### System Service (Optional)

Create `/etc/systemd/system/trading-hive.service`:
```ini
[Unit]
Description=TradingHive Trading Bot
After=network.target

[Service]
Type=simple
User=tradingbot
WorkingDirectory=/home/tradingbot/TradingHive-Rust-V17/TradingHive-Rust-V17
EnvironmentFile=/home/tradingbot/TradingHive-Rust-V17/TradingHive-Rust-V17/.env
ExecStart=/home/tradingbot/TradingHive-Rust-V17/TradingHive-Rust-V17/target/release/trading-hive session
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
```

Enable and start:
```bash
sudo systemctl daemon-reload
sudo systemctl enable trading-hive
sudo systemctl start trading-hive
sudo systemctl status trading-hive
```

---

## macOS Installation

### Step 1: Install Rust

```bash
# Download and run rustup installer
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Follow prompts (press Enter for default)
# Source the environment
source $HOME/.cargo/env

# Verify installation
rustc --version
cargo --version
```

### Step 2: Install Dependencies

Using Homebrew (recommended):
```bash
# Install Homebrew if not already installed
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

# Install Git and build tools
brew install git
```

Or using MacPorts:
```bash
sudo port install git
```

### Step 3: Clone TradingHive

```bash
git clone https://github.com/yourusername/TradingHive-Rust-V17.git
cd TradingHive-Rust-V17/TradingHive-Rust-V17
```

### Step 4: Set Up Environment

```bash
# Copy example environment file
cp .env.example .env

# Edit with your preferred editor
nano .env  # or: vim .env, code .env
```

Add your Alpaca credentials:
```env
APCA_API_KEY_ID=your_paper_key
APCA_API_SECRET_KEY=your_paper_secret
TRADING_HIVE_LIVE_CONFIRM=NO
```

### Step 5: Build the Project

```bash
# Build in release mode (optimized)
cargo build --release

# This takes 2-5 minutes on first build
# Uses about 1-2 GB disk space
```

### Step 6: Verify Installation

```bash
# Run tests
cargo test --all

# Should see: test result: ok. 11 passed
```

### Quick Run

```bash
# Paper trading (safe)
cargo run --release -- session

# Or use the built binary directly
./target/release/trading-hive session
```

### LaunchAgent Setup (Optional - Auto-start)

Create `~/Library/LaunchAgents/com.tradinghive.plist`:
```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.tradinghive.bot</string>
    <key>ProgramArguments</key>
    <array>
        <string>/path/to/TradingHive-Rust-V17/TradingHive-Rust-V17/target/release/trading-hive</string>
        <string>session</string>
    </array>
    <key>WorkingDirectory</key>
    <string>/path/to/TradingHive-Rust-V17/TradingHive-Rust-V17</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>APCA_API_KEY_ID</key>
        <string>your_paper_key</string>
        <key>APCA_API_SECRET_KEY</key>
        <string>your_paper_secret</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/tradinghive.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/tradinghive.error.log</string>
</dict>
</plist>
```

Enable:
```bash
launchctl load ~/Library/LaunchAgents/com.tradinghive.plist
launchctl start com.tradinghive.bot
launchctl list | grep tradinghive
```

---

## Post-Installation

### 1. Get Alpaca Credentials

1. Go to [app.alpaca.markets](https://app.alpaca.markets)
2. Sign up for free paper trading account
3. Navigate to Settings → API Keys
4. Copy your API Key ID and Secret Key
5. Add to your `.env` file:
```env
APCA_API_KEY_ID=your_key_here
APCA_API_SECRET_KEY=your_secret_here
```

### 2. Verify Your Setup

Run the verification script:

**Windows:**
```powershell
cargo test --all
```

**Linux/macOS:**
```bash
cargo test --all
```

Expected output:
```
running 11 tests
...
test result: ok. 11 passed
```

### 3. First Test Run

```bash
# All platforms - Run in paper trading mode (safe)
cargo run --release -- paper-smoke SPY

# This will:
# - Connect to Alpaca API
# - Fetch current market data
# - Submit a test paper order
# - Print results
```

### 4. Configure for Regular Use

Edit `.env` with your preferred settings:

```env
# Keep at NO for paper trading
TRADING_HIVE_LIVE_CONFIRM=NO

# Database location (relative path works)
TRADING_HIVE_DB=trading_hive.sqlite

# Risk parameters (0.0 = disabled)
TRADING_STOP_LOSS_PCT=0.05        # 5% stop loss
TRADING_TAKE_PROFIT_PCT=0.0       # Take profit disabled

# Optional: Market timezone (defaults to America/New_York)
TRADING_MARKET_TIMEZONE=America/New_York
```

---

## Verification

### Check Rust Version
```bash
rustc --version
# Should be 1.70+
```

### Check Cargo Version
```bash
cargo --version
```

### Run All Tests
```bash
cargo test --all
```

### Check Code Quality
```bash
cargo fmt --all -- --check
cargo clippy --all
```

### Build for Release
```bash
cargo build --release
```

### Verify Binary Works
```bash
# Windows
.\target\release\trading-hive.exe --help

# Linux/macOS
./target/release/trading-hive --help
```

### Check Database
```bash
# After first run, verify database was created
# Windows
dir trading_hive.sqlite

# Linux/macOS
ls -la trading_hive.sqlite
```

---

## Troubleshooting

### "rustc not found"
- **Windows**: Restart your terminal and system
- **Linux/macOS**: Run `source $HOME/.cargo/env`

### "can't find crate for `std`"
```bash
rustup update stable --force-non-host
```

### "Permission denied" (Linux/macOS)
```bash
chmod +x target/release/trading-hive
```

### "Database is locked"
- Ensure only one trading session is running
- Check for stale processes: `pkill -f trading-hive`

### Build takes too long
- First build is slow (downloads dependencies)
- Subsequent builds are faster
- Use `cargo build --release` for optimized binary

### Out of disk space
- Release build needs 2+ GB
- Free up space: `cargo clean` removes build artifacts

### Connection refused to Alpaca API
- Check `.env` file has correct credentials
- Verify Alpaca API status at [status.alpaca.markets](https://status.alpaca.markets)
- Check internet connection

### "TRADING_HIVE_LIVE_CONFIRM required"
- You tried to run live trading
- Never do this by accident
- To go live: Set `TRADING_HIVE_LIVE_CONFIRM=YES` in `.env` and use live credentials

---

## Getting Help

1. **Check the logs**: Review `hive-3hr.log` after running
2. **Run tests**: `cargo test --all --nocapture`
3. **Verify Alpaca**: Check [status.alpaca.markets](https://status.alpaca.markets)
4. **Check configuration**: Verify `.env` file is correct
5. **Review error messages**: Read error output carefully

---

## What's Next?

After successful installation:

1. **Run paper trading**: Start with `cargo run --release -- session`
2. **Monitor activity**: Check `trading_hive.sqlite` database for positions
3. **Review logs**: Check `hive-3hr.log` for trading activity
4. **Test commands**: Try `cargo run --release -- strategies` to see available strategies
5. **Read documentation**: Check `README.md` for more options

---

## System Requirements Summary

| OS | Min RAM | Min Disk | Recommended |
|----|---------|----------|-------------|
| Windows 10/11 | 8 GB | 3 GB | 16 GB RAM, SSD |
| Linux | 4 GB | 2 GB | 8 GB RAM, SSD |
| macOS 10.15+ | 4 GB | 2 GB | 8 GB RAM, SSD |

---

## Platform-Specific Notes

**Windows**: 
- Use PowerShell for best compatibility
- Git Bash also works
- Windows Terminal recommended

**Linux**: 
- Tested on Ubuntu 18.04, 20.04, 22.04
- Works on Debian, Fedora, Arch, etc.
- Systemd service file provided above

**macOS**: 
- Works on Intel and Apple Silicon (ARM)
- Homebrew recommended for dependencies
- LaunchAgent for automatic startup provided above

---

**Version**: 0.9.0  
**Last Updated**: 2026-09-02
