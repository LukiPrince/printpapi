<?php
/**
 * Plugin Name:       printpapi for WooCommerce
 * Description:       Prints a packing slip on a real printer when an order comes in, through a
 *                    self-hosted printpapi server. No cloud print service, no per-print fee.
 * Version:           1.0.0
 * Requires at least: 6.0
 * Requires PHP:      7.4
 * License:           MIT
 * License URI:       https://opensource.org/licenses/MIT
 * Text Domain:       printpapi
 */

if (!defined('ABSPATH')) {
    exit; // called directly — not from WordPress
}

const PRINTPAPI_OPTION = 'printpapi_settings';
const PRINTPAPI_EVENT  = 'printpapi_send_order';

// Everything here goes through the order CRUD getters, so HPOS (custom order tables) is fine.
add_action('before_woocommerce_init', function () {
    if (class_exists(\Automattic\WooCommerce\Utilities\FeaturesUtil::class)) {
        \Automattic\WooCommerce\Utilities\FeaturesUtil::declare_compatibility(
            'custom_order_tables', __FILE__, true);
    }
});

function printpapi_settings(): array
{
    return wp_parse_args(get_option(PRINTPAPI_OPTION, []), [
        'server_url' => '',
        'api_key'    => '',
        'printer_id' => '',
        'trigger'    => 'processing',
        'copies'     => 1,
    ]);
}

/* ---------------------------------------------------------------- printing */

/**
 * Order -> the JSON printpapi's POST /orders takes. Shaped like the WooCommerce REST order, which
 * is what the server's `format: woocommerce` mapper expects.
 */
function printpapi_order_payload(WC_Order $order): array
{
    $ship = $order->get_address('shipping');
    if (empty($ship['first_name']) && empty($ship['last_name'])) {
        $ship = $order->get_address('billing');   // digital/pickup orders carry no shipping address
    }

    $lines = [];
    foreach ($order->get_items() as $item) {
        $product = $item->get_product();
        $lines[] = [
            'name'     => $item->get_name(),
            'quantity' => $item->get_quantity(),
            'sku'      => $product ? $product->get_sku() : '',
            'total'    => $order->get_line_total($item, true),   // incl. tax, as displayed
        ];
    }

    $created = $order->get_date_created();

    return [
        'id'             => $order->get_id(),
        'number'         => $order->get_order_number(),
        'date_created'   => $created ? $created->date('Y-m-d') : '',
        'currency'       => $order->get_currency(),
        'total'          => $order->get_total(),
        'shipping_total' => $order->get_shipping_total(),
        'total_tax'      => $order->get_total_tax(),
        'discount_total' => $order->get_discount_total(),
        'customer_note'  => $order->get_customer_note(),
        'billing'        => [
            'email' => $order->get_billing_email(),
            'phone' => $order->get_billing_phone(),
        ],
        'shipping'       => [
            'first_name' => $ship['first_name'] ?? '',
            'last_name'  => $ship['last_name'] ?? '',
            'address_1'  => $ship['address_1'] ?? '',
            'address_2'  => $ship['address_2'] ?? '',
            'postcode'   => $ship['postcode'] ?? '',
            'city'       => $ship['city'] ?? '',
            'state'      => $ship['state'] ?? '',
            'country'    => $ship['country'] ?? '',
        ],
        'line_items'     => $lines,
    ];
}

/**
 * POST the order to printpapi. $idempotency_key null = an explicit reprint, which must print
 * even if this order was printed before.
 */
function printpapi_send_order(int $order_id, ?string $idempotency_key = null)
{
    $cfg = printpapi_settings();
    $order = wc_get_order($order_id);
    if (!$order || $cfg['server_url'] === '' || $cfg['api_key'] === '' || $cfg['printer_id'] === '') {
        return new WP_Error('printpapi_unconfigured', __('printpapi is not configured', 'printpapi'));
    }

    $body = [
        'printer_id' => (int) $cfg['printer_id'],
        'format'     => 'woocommerce',
        'order'      => printpapi_order_payload($order),
        'title'      => sprintf('Packing slip %s', $order->get_order_number()),
        'copies'     => max(1, (int) $cfg['copies']),
    ];
    if ($idempotency_key !== null) {
        // A retried hook (or a status flapping back and forth) must not print a second slip.
        $body['idempotency_key'] = $idempotency_key;
    }

    $response = wp_remote_post(rtrim($cfg['server_url'], '/') . '/orders', [
        'timeout' => 15,
        'headers' => [
            'Content-Type'  => 'application/json',
            'Authorization' => 'Bearer ' . $cfg['api_key'],
        ],
        'body'    => wp_json_encode($body),
    ]);

    if (is_wp_error($response)) {
        $order->add_order_note(sprintf(__('printpapi: %s', 'printpapi'), $response->get_error_message()));
        return $response;
    }
    $code = wp_remote_retrieve_response_code($response);
    $decoded = json_decode(wp_remote_retrieve_body($response), true);
    if ($code !== 200) {
        $message = is_array($decoded) && isset($decoded['error']) ? $decoded['error'] : "HTTP $code";
        $order->add_order_note(sprintf(__('printpapi: print failed — %s', 'printpapi'), $message));
        return new WP_Error('printpapi_http', $message);
    }
    $order->add_order_note(sprintf(__('printpapi: packing slip queued (job %s)', 'printpapi'),
        $decoded['job_id'] ?? '?'));
    return $decoded['job_id'] ?? true;
}
add_action(PRINTPAPI_EVENT, 'printpapi_send_order', 10, 2);

/** Queue the print in the background — checkout must never wait on a printer. */
function printpapi_schedule(int $order_id, ?string $key)
{
    wp_schedule_single_event(time(), PRINTPAPI_EVENT, [$order_id, $key]);
}

foreach (['processing', 'completed', 'on-hold'] as $status) {
    add_action("woocommerce_order_status_$status", function ($order_id) use ($status) {
        if (printpapi_settings()['trigger'] === $status) {
            printpapi_schedule((int) $order_id, "woo-$order_id-$status");
        }
    });
}

add_action('woocommerce_new_order', function ($order_id) {
    if (printpapi_settings()['trigger'] === 'new') {
        printpapi_schedule((int) $order_id, "woo-$order_id-new");
    }
});

/* --------------------------------------------------- reprint from the admin */

add_filter('woocommerce_order_actions', function ($actions) {
    $actions['printpapi_reprint'] = __('Print packing slip (printpapi)', 'printpapi');
    return $actions;
});

add_action('woocommerce_order_action_printpapi_reprint', function ($order) {
    printpapi_send_order($order->get_id(), null);   // no idempotency key: a reprint must print
});

/* ------------------------------------------------------------------ settings */

add_action('admin_menu', function () {
    add_submenu_page('woocommerce', __('printpapi', 'printpapi'), __('printpapi', 'printpapi'),
        'manage_woocommerce', 'printpapi', 'printpapi_settings_page');
});

add_action('admin_init', function () {
    register_setting('printpapi', PRINTPAPI_OPTION, ['sanitize_callback' => 'printpapi_sanitize']);
});

function printpapi_sanitize($input): array
{
    return [
        'server_url' => esc_url_raw(trim($input['server_url'] ?? ''), ['http', 'https']),
        'api_key'    => sanitize_text_field($input['api_key'] ?? ''),
        'printer_id' => (string) absint($input['printer_id'] ?? 0),
        'trigger'    => in_array($input['trigger'] ?? '', ['new', 'processing', 'completed', 'on-hold'], true)
            ? $input['trigger'] : 'processing',
        'copies'     => max(1, min(10, absint($input['copies'] ?? 1))),
    ];
}

/** Printers from the server, for the dropdown. [] when unreachable — never fatal. */
function printpapi_fetch_printers(array $cfg): array
{
    if ($cfg['server_url'] === '' || $cfg['api_key'] === '') {
        return [];
    }
    $response = wp_remote_get(rtrim($cfg['server_url'], '/') . '/printers', [
        'timeout' => 8,
        'headers' => ['Authorization' => 'Bearer ' . $cfg['api_key']],
    ]);
    if (is_wp_error($response) || wp_remote_retrieve_response_code($response) !== 200) {
        return [];
    }
    $decoded = json_decode(wp_remote_retrieve_body($response), true);
    return is_array($decoded) && isset($decoded['printers']) ? $decoded['printers'] : [];
}

function printpapi_settings_page()
{
    $cfg = printpapi_settings();
    $printers = printpapi_fetch_printers($cfg);
    ?>
    <div class="wrap">
        <h1><?php esc_html_e('printpapi', 'printpapi'); ?></h1>
        <p><?php esc_html_e('Prints a packing slip on your own printer when an order reaches the chosen status.', 'printpapi'); ?></p>
        <form method="post" action="options.php">
            <?php settings_fields('printpapi'); ?>
            <table class="form-table" role="presentation">
                <tr>
                    <th scope="row"><label for="printpapi_url"><?php esc_html_e('Server URL', 'printpapi'); ?></label></th>
                    <td><input name="<?php echo esc_attr(PRINTPAPI_OPTION); ?>[server_url]" id="printpapi_url"
                               type="url" class="regular-text" placeholder="https://print.example.com"
                               value="<?php echo esc_attr($cfg['server_url']); ?>"></td>
                </tr>
                <tr>
                    <th scope="row"><label for="printpapi_key"><?php esc_html_e('API key', 'printpapi'); ?></label></th>
                    <td><input name="<?php echo esc_attr(PRINTPAPI_OPTION); ?>[api_key]" id="printpapi_key"
                               type="password" class="regular-text" autocomplete="off"
                               value="<?php echo esc_attr($cfg['api_key']); ?>">
                        <p class="description"><?php esc_html_e('A client key from the printpapi dashboard — not the root token.', 'printpapi'); ?></p></td>
                </tr>
                <tr>
                    <th scope="row"><label for="printpapi_printer"><?php esc_html_e('Printer', 'printpapi'); ?></label></th>
                    <td>
                        <?php if ($printers) : ?>
                            <select name="<?php echo esc_attr(PRINTPAPI_OPTION); ?>[printer_id]" id="printpapi_printer">
                                <?php foreach ($printers as $printer) : ?>
                                    <option value="<?php echo esc_attr($printer['id']); ?>"
                                        <?php selected((string) $printer['id'], $cfg['printer_id']); ?>
                                        <?php disabled(empty($printer['can_pdf'])); ?>>
                                        <?php echo esc_html($printer['name'] . (empty($printer['can_pdf']) ? ' — raw only' : '')); ?>
                                    </option>
                                <?php endforeach; ?>
                            </select>
                            <p class="description"><?php esc_html_e('Only PDF-capable printers can print a packing slip.', 'printpapi'); ?></p>
                        <?php else : ?>
                            <input name="<?php echo esc_attr(PRINTPAPI_OPTION); ?>[printer_id]" id="printpapi_printer"
                                   type="number" min="1" value="<?php echo esc_attr($cfg['printer_id']); ?>">
                            <p class="description"><?php esc_html_e('Enter the printer id — the server could not be reached to list printers.', 'printpapi'); ?></p>
                        <?php endif; ?>
                    </td>
                </tr>
                <tr>
                    <th scope="row"><label for="printpapi_trigger"><?php esc_html_e('Print when', 'printpapi'); ?></label></th>
                    <td><select name="<?php echo esc_attr(PRINTPAPI_OPTION); ?>[trigger]" id="printpapi_trigger">
                            <?php foreach ([
                                'new'        => __('the order is created', 'printpapi'),
                                'processing' => __('the order is processing (paid)', 'printpapi'),
                                'on-hold'    => __('the order is on hold', 'printpapi'),
                                'completed'  => __('the order is completed', 'printpapi'),
                            ] as $value => $label) : ?>
                                <option value="<?php echo esc_attr($value); ?>" <?php selected($value, $cfg['trigger']); ?>>
                                    <?php echo esc_html($label); ?>
                                </option>
                            <?php endforeach; ?>
                        </select></td>
                </tr>
                <tr>
                    <th scope="row"><label for="printpapi_copies"><?php esc_html_e('Copies', 'printpapi'); ?></label></th>
                    <td><input name="<?php echo esc_attr(PRINTPAPI_OPTION); ?>[copies]" id="printpapi_copies"
                               type="number" min="1" max="10" value="<?php echo esc_attr($cfg['copies']); ?>"></td>
                </tr>
            </table>
            <?php submit_button(); ?>
        </form>
    </div>
    <?php
}
