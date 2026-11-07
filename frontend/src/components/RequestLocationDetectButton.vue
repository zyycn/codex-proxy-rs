<script setup lang="ts">
import { ZIconButton } from '@codex-proxy/ui'

import { LocateFixed } from '@lucide/vue'
import { useI18n } from 'vue-i18n'

defineProps<{ loading: boolean, disabled?: boolean }>()

const emit = defineEmits<{ detect: [] }>()

const { t } = useI18n({ useScope: 'global' })
</script>

<template>
  <ZIconButton
    size="default"
    :aria-label="loading ? t('common.location.resolving') : t('common.location.detect')"
    :aria-busy="loading"
    :disabled="disabled || loading"
    @click="emit('detect')"
  >
    <span class="relative grid size-7 place-items-center text-z-link">
      <span v-if="loading" class="location-ripple" aria-hidden="true" />
      <span v-if="loading" class="location-ripple location-ripple-delayed" aria-hidden="true" />
      <LocateFixed class="relative z-10 size-4" aria-hidden="true" />
    </span>
  </ZIconButton>
</template>

<style scoped>
.location-ripple {
  position: absolute;
  width: 1.25rem;
  height: 1.25rem;
  border: 1px solid currentColor;
  border-radius: 50%;
  opacity: 0;
  pointer-events: none;
  animation: location-ripple 1.8s ease-out infinite forwards;
}

.location-ripple-delayed {
  animation-delay: 0.9s;
}

@keyframes location-ripple {
  from {
    opacity: 0.3;
    transform: scale(0.6);
  }

  to {
    opacity: 0;
    transform: scale(1.55);
  }
}

@media (prefers-reduced-motion: reduce) {
  .location-ripple {
    animation: none;
    opacity: 0.4;
    transform: scale(1.1);
  }

  .location-ripple-delayed {
    display: none;
  }
}
</style>
