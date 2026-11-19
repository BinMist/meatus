<template>
  <SlNotificationViewport position="top-right" label="系统通知" class="mt-notification-viewport">
    <TransitionGroup name="mt-notification" tag="div" class="mt-notification-list">
      <SlNotification
        v-for="item in notifications.items"
        :key="item.id"
        :status="item.status"
        :title="item.title"
        :description="item.description"
        :timestamp="item.timestamp"
        :auto-dismiss-duration="item.duration"
        dismissible
        close-label="关闭通知"
        @dismiss="dismissNotification(item.id)"
      />
    </TransitionGroup>
  </SlNotificationViewport>
</template>

<script setup lang="ts">
import { SlNotification, SlNotificationViewport } from '@sailonix/ui/notification'
import { dismissNotification, notifications } from '../services/notifications'
</script>

<style scoped>
.mt-notification-viewport {
  top: 84px;
  /* 余量必须 ≥ 阴影的「偏移 + 模糊」外扩（当前 10 + 30 = 40px），
     否则滚动容器的矩形裁剪会把阴影切成一条直边。 */
  --sl-notification-shadow-bleed: 48px;
}

.mt-notification-list {
  display: flex;
  flex-direction: column;
  gap: 12px;
  width: min(380px, calc(100vw - 32px));
}

:deep(.sl-notification) {
  /* 偏移收拢到 10px：18px 偏移 + 44px 模糊会把阴影甩到卡片下方，
     看着像卡片下又挂了一条灰板；10+30 的外扩是 40px，边缘刚好贴合卡片轮廓。 */
  box-shadow: 0 10px 30px rgba(31, 44, 49, 0.14);
}

.mt-notification-enter-active,
.mt-notification-leave-active {
  transition: opacity 0.18s ease, transform 0.18s ease;
}

.mt-notification-enter-from {
  opacity: 0;
  transform: translateY(-8px);
}

.mt-notification-leave-to {
  opacity: 0;
  transform: translateX(12px);
}

@media (max-width: 900px) {
  .mt-notification-viewport {
    top: 72px;
  }
}

@media (max-width: 520px) {
  .mt-notification-viewport {
    top: 64px;
    padding: 10px;
  }

  .mt-notification-list {
    width: 100%;
  }
}
</style>
