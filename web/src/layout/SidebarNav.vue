<template>
  <nav class="rail-nav" aria-label="主导航">
    <div v-for="group in groups" :key="group.label" class="nav-group">
      <div class="nav-group-title">{{ group.label }}</div>
      <div class="nav-list">
        <sl-cell
          v-for="item in group.items"
          :key="item.id"
          class="nav-item"
          :class="{ 'is-active': activePath === item.id }"
          :aria-current="activePath === item.id ? 'page' : undefined"
          :title="item.label"
          :arrow="false"
          clickable
          @click="emit('navigate', item.id)"
        >
          <template #icon>
            <sl-icon class="nav-ico" :name="item.icon" :size="17" />
          </template>
          <template #title>
            <span class="nav-label">{{ item.label }}</span>
          </template>
        </sl-cell>
      </div>
    </div>
  </nav>
</template>

<script setup lang="ts">
import { SlIcon } from '@sailonix/icon'
import { SlCell } from '@sailonix/ui/cell'
import type { NavGroup } from './types'

defineOptions({ name: 'SidebarNav' })

defineProps<{
  groups: NavGroup[]
  activePath: string
}>()

const emit = defineEmits<{
  navigate: [path: string]
}>()
</script>
