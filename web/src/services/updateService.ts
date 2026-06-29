// 快泛claw 官网版本 API 配置
import { invoke } from '@tauri-apps/api/core';

const KUAFAN_API = 'https://kuaifanclaw.cn';

export interface ReleaseInfo {
  tag_name: string;
  version: string;
  name: string;
  body: string;
  published_at: string;
  assets: { name: string; browser_download_url: string }[];
  is_latest: boolean;
}

interface VersionInfo {
  latestVersion: string;
  hasUpdate: boolean;
  downloadUrl: string;
  changelog: string;
}

// 官网 /api/public/packages 返回的原始格式
interface KuaifanPackage {
  id: number;
  name: string;
  platform: string;
  package_type: string;
  version: string;
  file_size: number;
  download_url: string;
  project_id: number;
  project_name: string;
  created_at: string;
}

class UpdateService {
  // 从快泛claw官网获取最新版本信息
  async fetchLatestVersion(): Promise<ReleaseInfo | null> {
    try {
      const releases = await this.fetchRecentReleases(1);
      return releases[0] || null;
    } catch (error) {
      console.error('获取最新版本失败:', error);
      return null;
    }
  }

  // 从快泛claw官网获取所有版本（最新 N 个）
  async fetchRecentReleases(count: number = 3): Promise<ReleaseInfo[]> {
    try {
      const json = await invoke<string>('fetch_versions');
      const allPackages: KuaifanPackage[] = JSON.parse(json);
      const packages = allPackages.filter(p => p.platform === 'win');

      // 按版本分组，同版本下多个平台作为多个 assets
      const versionMap = new Map<string, KuaifanPackage[]>();
      for (const pkg of packages) {
        const existing = versionMap.get(pkg.version) || [];
        existing.push(pkg);
        versionMap.set(pkg.version, existing);
      }

      // 转换为 ReleaseInfo 格式，取最新 N 个版本
      const releases: ReleaseInfo[] = [];
      const sortedVersions = [...versionMap.keys()].sort((a, b) =>
        this.compareVersions(b, a)
      );

      for (let i = 0; i < Math.min(count, sortedVersions.length); i++) {
        const ver = sortedVersions[i];
        const pkgs = versionMap.get(ver)!;
        releases.push({
          tag_name: `v${ver}`,
          version: ver,
          name: pkgs[0].name,
          body: '',
          published_at: pkgs[0].created_at,
          assets: pkgs.map(p => ({
            name: p.download_url.split('/').pop() || p.name,
            browser_download_url: `${KUAFAN_API}${p.download_url}`,
          })),
          is_latest: i === 0,
        });
      }

      return releases;
    } catch (error) {
      console.error('获取版本列表失败:', error);
      return [];
    }
  }

  // 获取可下载的 exe 文件信息
  getExeAsset(release: ReleaseInfo): { name: string; url: string } | null {
    const exeAsset = release.assets.find(
      a => a.name.endsWith('-setup.exe') || a.name.endsWith('.exe')
    );
    if (exeAsset) {
      return { name: exeAsset.name, url: exeAsset.browser_download_url };
    }
    return null;
  }

  // 检查应用版本 - 兼容旧接口
  async checkAppVersion(currentVersion: string): Promise<VersionInfo> {
    const latest = await this.fetchLatestVersion();
    if (latest) {
      const current = currentVersion.replace('v', '');
      return {
        latestVersion: latest.version,
        hasUpdate: this.compareVersions(latest.version, current) > 0,
        downloadUrl: this.getExeAsset(latest)?.url || '',
        changelog: latest.body || latest.name,
      };
    }
    return {
      latestVersion: currentVersion,
      hasUpdate: false,
      downloadUrl: '',
      changelog: '',
    };
  }

  // 检查OpenClaw版本
  async checkOpenClawVersion(currentVersion: string): Promise<VersionInfo> {
    return {
      latestVersion: currentVersion,
      hasUpdate: false,
      downloadUrl: '',
      changelog: '',
    };
  }

  // 下载并安装更新
  async downloadAndInstallUpdate(url: string): Promise<boolean> {
    try {
      if (!url) {
        console.error('下载链接为空');
        return false;
      }
      console.log('开始下载更新:', url);
      await invoke('download_update', { url });
      return true;
    } catch (error) {
      console.error('更新下载失败:', error);
      return false;
    }
  }

  // 版本比较: 返回正数表示 v1 > v2
  compareVersions(v1: string, v2: string): number {
    const parts1 = v1.split('.').map(Number);
    const parts2 = v2.split('.').map(Number);
    for (let i = 0; i < Math.max(parts1.length, parts2.length); i++) {
      const p1 = parts1[i] || 0;
      const p2 = parts2[i] || 0;
      if (p1 > p2) return 1;
      if (p1 < p2) return -1;
    }
    return 0;
  }
}

export const updateService = new UpdateService();
