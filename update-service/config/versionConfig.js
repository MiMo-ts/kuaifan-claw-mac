// 版本管理配置
// 当发布新版本时，更新此文件中的版本信息

module.exports = {
  // 应用版本信息
  app: {
    current: '1.0.0',
    // 历史版本记录
    history: [
      {
        version: '1.0.0',
        releaseDate: '2026-06-30',
        changelog: '\n• 微信/企业微信插件修复与加固\n• 安全增强：HTTPS更新通道、npmSpec校验、路径遍历防护\n• 插件SDK子路径模块补全\n• 版本统一为1.0.0',
        downloadUrl: 'https://kuaifanclaw.cn/download/kuafan-claw-1.0.0.zip'
      }
    ]
  },
  
  // OpenClaw版本信息
  openclaw: {
    current: '1.0.0',
    // 历史版本记录
    history: [
      {
        version: '1.0.0',
        releaseDate: '2026-06-30',
        changelog: '\n• 初始版本发布\n• 支持多平台机器人\n• 优化网关性能',
        downloadUrl: 'https://kuaifanclaw.cn/download/openclaw-cn-1.0.0.zip'
      }
    ]
  }
};
