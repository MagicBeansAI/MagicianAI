#include <stdio.h>
#include "esp_heap_caps.h"
#include "esp_wifi.h"
#include "esp_event.h"
#include "esp_netif.h"
#include "nvs_flash.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

static void rep(const char *phase){
    printf("HEAP %-26s free=%6u  largest_block=%6u\n", phase,
        (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL),
        (unsigned)heap_caps_get_largest_free_block(MALLOC_CAP_INTERNAL));
}
void app_main(void){
    printf("\n=== HEAP BUDGET (Task 0.3) ===\n");
    rep("boot");
    nvs_flash_init();                     rep("after nvs_flash_init");
    esp_netif_init();                     rep("after esp_netif_init");
    esp_event_loop_create_default();      rep("after event loop");
    esp_netif_create_default_wifi_sta();  rep("after netif sta");
    wifi_init_config_t c = WIFI_INIT_CONFIG_DEFAULT();
    esp_wifi_init(&c);                    rep("after esp_wifi_init");
    esp_wifi_set_mode(WIFI_MODE_STA);
    esp_wifi_start();                     rep("after esp_wifi_start");
    vTaskDelay(pdMS_TO_TICKS(1500));
    rep("settled (WiFi up, no AP)");
    printf("\nNOTE: a mbedTLS session costs roughly another 30-50 KB on top.\n");
    printf("BUDGET COMPLETE\n");
    while(1) vTaskDelay(pdMS_TO_TICKS(1000));
}
