//! Taxas do Mercado em centavos inteiros (docs/conhecimento.md 5.1, pesquisa-web.md 4).
//! Nunca dividir/multiplicar por 1,15: erra 1 centavo.

/// Taxa da Steam (5 %) e do publisher (10 %), cada uma arredondada para baixo, mínimo 1 centavo.
/// `seller_cents` é o líquido do vendedor.
pub fn fees(seller_cents: i64) -> (i64, i64) {
    (
        (seller_cents * 5 / 100).max(1),
        (seller_cents * 10 / 100).max(1),
    )
}

/// O que o comprador paga para o vendedor receber `seller_cents`. `0` ou menos → `0`.
pub fn buyer_pays(seller_cents: i64) -> i64 {
    if seller_cents <= 0 {
        return 0;
    }
    let (steam, publisher) = fees(seller_cents);
    seller_cents + steam + publisher
}

/// Maior líquido `s` tal que `buyer_pays(s) <= buyer_cents` (busca binária; a função é crescente).
/// Abaixo de 3 centavos (1+1+1) ninguém recebe nada: `0`.
pub fn seller_gets(buyer_cents: i64) -> i64 {
    let (mut lo, mut hi) = (0, buyer_cents.max(0));
    while lo < hi {
        let mid = lo + (hi - lo + 1) / 2;
        if buyer_pays(mid) <= buyer_cents {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_pairs_from_steam() {
        assert_eq!(fees(1398), (69, 139));
        assert_eq!(buyer_pays(1398), 1606);
        assert_eq!(buyer_pays(589), 676); // Moonrise BS
        assert_eq!(fees(512), (25, 51)); // unPrice 512
        assert_eq!(fees(523), (26, 52));
    }

    #[test]
    fn erick_flip_net() {
        // comprador R$ 12,52 → vendedor R$ 10,89 (1089+54+108 = 1251; 1090 daria 1253)
        assert_eq!(seller_gets(1252), 1089);
        assert_eq!(buyer_pays(1089), 1251);
        assert_eq!(buyer_pays(1090), 1253);
    }

    #[test]
    fn edges() {
        assert_eq!(buyer_pays(0), 0);
        assert_eq!(buyer_pays(-5), 0);
        assert_eq!(buyer_pays(1), 3); // mínimo 1+1+1
        assert_eq!(seller_gets(0), 0);
        assert_eq!(seller_gets(2), 0);
        assert_eq!(seller_gets(3), 1);
        assert_eq!(seller_gets(-10), 0);
    }

    #[test]
    fn inverse_is_exact_for_every_price() {
        let mut prev = 0;
        for b in 0..=20_000 {
            let s = seller_gets(b);
            assert!(s >= prev, "líquido não pode cair quando o preço sobe");
            prev = s;
            if s > 0 {
                assert!(buyer_pays(s) <= b);
            }
            assert!(buyer_pays(s + 1) > b, "b={b}: s={s} não é o maior");
        }
    }
}
