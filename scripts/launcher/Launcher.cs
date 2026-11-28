// Local Windows launcher, GPL-3.0-or-later. Built with .NET Framework 4.x csc.
using System;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Windows.Forms;
using System.Xml.Linq;

internal static class Launcher
{
    static readonly string Base = AppDomain.CurrentDomain.BaseDirectory;
    static readonly string Home = Environment.GetEnvironmentVariable("VICTORY_LAUNCHER_HOME") ??
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "VictoryRTX2080");
    static string Game, Cache, Saves;
    [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int w, int h, uint flags);
    [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr value);
    [StructLayout(LayoutKind.Sequential)] struct Rect { public int Left, Top, Right, Bottom; }

    [STAThread] static int Main(string[] args)
    {
        bool check = args.Contains("--check");
        try {
            Application.EnableVisualStyles();
            string settings = Path.Combine(Home, "launcher.xml");
            if (File.Exists(settings)) {
                var xml = XElement.Load(settings);
                Game = (string)xml.Element("game"); Cache = (string)xml.Element("cache"); Saves = (string)xml.Element("saves");
            }
            if (args.Contains("--configure") || !File.Exists(Game)) {
                if (check) throw new Exception("Configure o jogo abrindo Iniciar-Victory.exe primeiro.");
                using (var picker = new OpenFileDialog()) {
                    picker.Title = "Selecione o executavel do seu pacote do jogo";
                    picker.Filter = "Executavel do jogo|game_scarlett_master.exe|Executaveis|*.exe";
                    picker.CheckFileExists = true;
                    if (picker.ShowDialog() != DialogResult.OK) return 0;
                    Game = picker.FileName;
                }
                string parent = Path.GetDirectoryName(Path.GetDirectoryName(Game));
                // Reuse the user's existing local data when those folders exist.
                if (String.IsNullOrWhiteSpace(Cache)) Cache = Directory.Exists(Path.Combine(parent, "victory-8gb-cache")) ?
                    Path.Combine(parent, "victory-8gb-cache") : Path.Combine(Home, "cache");
                if (String.IsNullOrWhiteSpace(Saves)) Saves = Directory.Exists(Path.Combine(parent, "victory-data")) ?
                    Path.Combine(parent, "victory-data") : Path.Combine(Home, "data");
                Directory.CreateDirectory(Home);
                new XElement("launcher", new XElement("game", Game), new XElement("cache", Cache), new XElement("saves", Saves)).Save(settings);
            }
            string runtime = new[] { Path.Combine(Base, "runtime", "victory.exe"),
                Path.Combine(Base, "target", "release", "victory-buffer-pass.exe"),
                Path.Combine(Base, "target", "release", "victory.exe") }.FirstOrDefault(File.Exists);
            if (runtime == null) throw new Exception("Runtime nao encontrado. Baixe e extraia o pacote Windows completo da Release; o ZIP de codigo nao inclui o runtime compilado.");
            if (String.IsNullOrWhiteSpace(Cache) || String.IsNullOrWhiteSpace(Saves)) throw new Exception("Configuracao incompleta. Abra com --configure.");
            if (check) {
                Console.WriteLine("Runtime=" + runtime + "\nGame=" + Game + "\nCache=" + Cache + "\nSaves=" + Saves + "\nDisplay=1280x720; Series S; buffer pass; profiling off");
                return 0;
            }
            Directory.CreateDirectory(Home); Directory.CreateDirectory(Cache); Directory.CreateDirectory(Saves);
            string config = Path.Combine(Home, "victory.toml");
            File.WriteAllText(config, "[paths]\ngame_dir = " + Toml(Path.GetDirectoryName(Game)) + "\ncache_dir = " + Toml(Cache) + "\ndata_dir = " + Toml(Saves) + "\n", new UTF8Encoding(false));
            string logs = Path.Combine(Home, "logs", DateTime.Now.ToString("yyyyMMdd-HHmmss-fff"));
            Directory.CreateDirectory(logs);
            var info = new ProcessStartInfo(runtime, "run " + Quote(Game) + " --execute --console series-s --unimplemented trap");
            info.WorkingDirectory = Path.GetDirectoryName(Game);
            info.UseShellExecute = false; info.CreateNoWindow = true;
            info.RedirectStandardOutput = true; info.RedirectStandardError = true;
            foreach (string key in info.EnvironmentVariables.Keys.Cast<string>().Where(k => k.StartsWith("VICTORY_", StringComparison.OrdinalIgnoreCase)).ToArray())
                info.EnvironmentVariables.Remove(key);
            string[,] env = {
                {"CONFIG", config}, {"DISPLAY_MODE", "1280x720"}, {"LOG", "info"},
                {"MAPPING_RANGES", "1"}, {"HEAP_PASS_NO_SELF_MARK", "1"}, {"PIPELINE_SMALL_FIRST", "1"},
                {"TEXTURE_DROP_MIPS", "1"}, {"SMALL_TEXTURE_KB", "0"}, {"QUEUE2", "1"},
                {"INDIRECT_WINDOW", "1"}, {"ASYNC_SUBMIT", "1"}, {"ASYNC_SUBMIT_SAFE", "1"},
                {"CARD_HASH_WIDE", "1"}, {"LATE_WAIT", "2"}, {"BACK_SETTLE", "0"}
            };
            for (int i = 0; i < env.GetLength(0); i++) info.EnvironmentVariables["VICTORY_" + env[i, 0]] = env[i, 1];
            string dxil = FindDxil();
            if (dxil != null) info.EnvironmentVariables["VICTORY_DXIL"] = dxil;
            using (var output = new StreamWriter(Path.Combine(logs, "stdout.txt")))
            using (var error = new StreamWriter(Path.Combine(logs, "stderr.txt")))
            using (var process = new Process { StartInfo = info }) {
                output.AutoFlush = true; error.AutoFlush = true;
                process.OutputDataReceived += (o, e) => { if (e.Data != null) output.WriteLine(e.Data); };
                process.ErrorDataReceived += (o, e) => { if (e.Data != null) error.WriteLine(e.Data); };
                process.Start(); process.BeginOutputReadLine(); process.BeginErrorReadLine();
                File.WriteAllText(Path.Combine(logs, "session.txt"), "Runtime=" + runtime + "\nPID=" + process.Id + "\nGame=" + Game + "\nConfig=" + config);
                for (int i = 0; i < 240 && !process.HasExited; i++) {
                    process.Refresh();
                    if (process.MainWindowHandle != IntPtr.Zero) { SizeWindow(process.MainWindowHandle); break; }
                    Thread.Sleep(250);
                }
                process.WaitForExit();
                if (process.ExitCode != 0) throw new Exception("O runtime encerrou com codigo " + process.ExitCode + ". Logs em:\n" + logs);
            }
            return 0;
        } catch (Exception ex) {
            if (check) Console.Error.WriteLine(ex.Message);
            else MessageBox.Show(ex.Message, "Victory RTX2080", MessageBoxButtons.OK, MessageBoxIcon.Error);
            return 1;
        }
    }
    static string Toml(string s) { return "\"" + s.Replace("\\", "\\\\").Replace("\"", "\\\"").Replace("\r", "\\r").Replace("\n", "\\n") + "\""; }
    static string Quote(string s) { return "\"" + s + "\""; } // A Windows filename cannot contain a quote.
    static string FindDxil() {
        string supplied = Environment.GetEnvironmentVariable("VICTORY_DXIL");
        if (File.Exists(supplied)) return supplied;
        string sdk = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ProgramFilesX86), "Windows Kits", "10", "bin");
        if (!Directory.Exists(sdk)) return null;
        return Directory.GetDirectories(sdk).OrderByDescending(p => p, StringComparer.OrdinalIgnoreCase)
            .Select(p => Path.Combine(p, "x64", "dxil.dll")).FirstOrDefault(File.Exists);
    }
    static void SizeWindow(IntPtr hwnd) {
        IntPtr old = SetThreadDpiAwarenessContext(new IntPtr(-4));
        try {
            Rect outer, client;
            if (GetWindowRect(hwnd, out outer) && GetClientRect(hwnd, out client))
                SetWindowPos(hwnd, IntPtr.Zero, 0, 0, 1280 + outer.Right - outer.Left - client.Right + client.Left,
                    720 + outer.Bottom - outer.Top - client.Bottom + client.Top, 0x0002 | 0x0004 | 0x0010);
        } finally { if (old != IntPtr.Zero) SetThreadDpiAwarenessContext(old); }
    }
}
