<template>
  <div class="detail-grid">
    <sl-panel title="签发链路" description="HTTP-01 / DNS-01 流程与缓存加载">
      <sl-timeline :items="timelineItems" />
    </sl-panel>
    <sl-panel title="证书目录" description="本地缓存，0600 权限" variant="filled">
      <sl-descriptions
        class="mt-cert-facts"
        :items="items"
        :column="1"
        layout="horizontal"
        bordered
        size="sm"
      />
    </sl-panel>
  </div>
</template>

<script setup lang="ts">
import { SlDescriptions } from '@sailonix/ui/descriptions'
import { SlPanel } from '@sailonix/ui/panel'
import { SlTimeline } from '@sailonix/ui/timeline'

defineOptions({ name: 'CertificateDetailPanels' })

defineProps<{ items: Array<{ label: string; value: string | number }> }>()

const timelineItems = [
  { title: 'HTTP-01 校验', content: '精确域名由 server 在 80 端口响应 /.well-known/acme-challenge', status: 'success' as const },
  { title: 'DNS-01 校验', content: '泛域名通过 Cloudflare / DNSPod 创建 _acme-challenge TXT，校验后自动清理', status: 'success' as const },
  { title: '证书签发', content: 'rustls-acme / instant-acme 完成 ACME 账户与证书申请', status: 'success' as const },
  { title: '缓存落盘', content: '证书以 0600 权限写入 cert_dir，到期前 30 天自动续期', status: 'success' as const },
  { title: 'TLS 热加载', content: '按 SNI 选择精确域名或泛域名 resolver，无需重启 server', status: 'success' as const },
  { title: '续期失败', content: '保留旧证书与旧 resolver，失败原因显示在域名行', status: 'warning' as const },
]
</script>
