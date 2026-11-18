import type { ProxyTypeOption } from './types'

export const proxyTypeOptions: ProxyTypeOption[] = [
  { value: 'tcp', label: 'TCP', description: '端口映射，适合 SSH / 数据库' },
  { value: 'udp', label: 'UDP', description: '无连接端口映射' },
  { value: 'http', label: 'HTTP', description: '按 Host 路由，必须配置域名' },
  { value: 'https', label: 'HTTPS', description: '按 SNI 路由，必须配置域名' },
  { value: 'socks5', label: 'SOCKS5', description: '复用 server 的 SOCKS5 监听' },
]

export const proxyTypeOptionsAll = [
  { value: 'all', label: '全部类型' },
  ...proxyTypeOptions.map((item) => ({ value: item.value, label: item.label })),
]
