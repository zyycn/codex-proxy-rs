<script setup lang="ts">
import { ZButton, ZDialog } from '@codex-proxy/ui'
import { onMounted, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { useCopyText } from '@/composables/useCopyText'

const props = defineProps<{ link: URL }>()
defineEmits<{ closed: [] }>()

const { t } = useI18n({ useScope: 'global' })
const copyText = useCopyText()
const open = shallowRef(false)

// 确认框按请求挂载，挂载后打开才会触发入场动画
onMounted(() => {
  open.value = true
})

function openLink() {
  window.open(props.link.href, '_blank', 'noopener,noreferrer')
  open.value = false
}
</script>

<template>
  <ZDialog
    v-model="open"
    :title="t('plugins.frame.openExternalLink')"
    width="28rem"
    @closed="$emit('closed')"
  >
    <template #description>
      <bdi dir="ltr" class="break-all">{{ link.host }}</bdi>
    </template>
    <template #footer>
      <ZButton @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton @click="copyText(link.href)">
        {{ t('plugins.frame.copyLink') }}
      </ZButton>
      <ZButton type="primary" @click="openLink">
        {{ t('plugins.frame.openLink') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
