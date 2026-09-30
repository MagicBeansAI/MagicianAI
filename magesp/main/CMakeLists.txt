idf_component_register(SRCS "maze.c" "net.c" "connection.c" "audio.c" "voice.c" "realtime.c"
    REQUIRES esp32_c6_touch_amoled_1_8
    PRIV_REQUIRES esp_wifi esp_netif esp_event nvs_flash esp_timer
                  esp_http_server esp_http_client esp-tls mbedtls
                  esp_websocket_client json)
