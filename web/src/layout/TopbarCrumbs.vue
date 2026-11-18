<template>
  <sl-breadcrumb class="crumbs" aria-label="当前位置">
    <sl-breadcrumb-item>控制面</sl-breadcrumb-item>
    <sl-breadcrumb-item>{{ groupLabel }}</sl-breadcrumb-item>
    <sl-breadcrumb-item current>{{ pageTitle }}</sl-breadcrumb-item>
  </sl-breadcrumb>
</template>

<script setup lang="ts">
import { SlBreadcrumb, SlBreadcrumbItem } from '@sailonix/ui/breadcrumb'

defineOptions({ name: 'TopbarCrumbs' })

defineProps<{
  groupLabel: string
  pageTitle: string
}>()
</script>
