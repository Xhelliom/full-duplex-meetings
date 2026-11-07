//! Reconnaît quand quelqu'un appelle l'agent par son prénom, son surnom ou son nom,
//! comme on interpelle un collègue (« Nova, tu peux… », « t'en penses quoi Martin ? »).

use unicode_normalization::UnicodeNormalization;

/// Minuscules, sans accents, ponctuation -> espaces.
pub fn normalize(text: &str) -> String {
    let s: String = text
        .nfkd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .flat_map(|c| c.to_lowercase())
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + (ca != *cb) as usize).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

pub struct NameDetector {
    /// Chaque nom est une suite de mots normalisés (« jean pierre », « nova »).
    names: Vec<Vec<String>>,
}

impl NameDetector {
    pub fn new<I: IntoIterator<Item = S>, S: AsRef<str>>(names: I) -> Self {
        let names = names
            .into_iter()
            .map(|n| normalize(n.as_ref()).split(' ').filter(|w| !w.is_empty()).map(String::from).collect::<Vec<_>>())
            .filter(|w: &Vec<String>| !w.is_empty())
            .collect();
        Self { names }
    }

    /// Mot transcrit ≈ mot du nom ? La transcription déforme souvent les prénoms rares
    /// (« Nova » -> « Novah ») : on tolère 1 faute à partir de 5 lettres.
    fn word_matches(heard: &str, name: &str) -> bool {
        if heard == name {
            return true;
        }
        let len = name.chars().count();
        len >= 5 && levenshtein(heard, name) <= 1
    }

    pub fn mentioned(&self, transcript: &str) -> bool {
        let norm = normalize(transcript);
        let words: Vec<&str> = norm.split(' ').filter(|w| !w.is_empty()).collect();
        self.names.iter().any(|name| {
            words.len() >= name.len()
                && words
                    .windows(name.len())
                    .any(|win| win.iter().zip(name).all(|(h, n)| Self::word_matches(h, n)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prenom_surnom_nom() {
        let d = NameDetector::new(["Nova", "Nov", "Nova Martin", "Martin"]);
        assert!(d.mentioned("Nova, tu peux résumer ?"));
        assert!(d.mentioned("t'en penses quoi Martin ?"));
        assert!(d.mentioned("Hé Nov !"));
        assert!(!d.mentioned("On reprend le budget"));
        assert!(!d.mentioned("c'est novateur")); // pas dans un autre mot
    }

    #[test]
    fn accents_casse_et_fautes() {
        let d = NameDetector::new(["Hélène", "Jean-Pierre"]);
        assert!(d.mentioned("HELENE tu es là"));
        assert!(d.mentioned("jean pierre ?"));
        assert!(d.mentioned("Hélènne, une question")); // 1 faute tolérée
        assert!(!d.mentioned("Jean est là"));
    }
}
