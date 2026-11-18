<template>
  <div class="detail-grid">
    <sl-panel title="签发链路" description="HTTP-01 流程与缓存加载">
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
  { title: 'HTTP-01 校验', content: 'server 在 80 端口响应 /.well-known/acme-challenge', status: 'success' as const },
  { title: '证书签发', content: 'rustls-acme 完成 ACME 账户与证书申请', status: 'success' as const },
  { title: '缓存落盘', content: '证书以 0600 权限写入 cert_dir', status: 'success' as const },
  { title: '解析与展示', content: '读取叶证书 SAN / 到期 / 序列号 / 链长度', status: 'success' as const },
  { title: 'TLS 热加载', content: '按 SNI 选择域名 resolver，无需重启 server', status: 'success' as const },
]
</script>
